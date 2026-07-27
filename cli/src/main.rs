use std::env;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use kitowall_backend::config::{
    is_supported_transition, normalize_pack_name, GenericJsonPack, Mode, PackConfig, RedditPack,
    StaticUrlPack, StringOrList, UnsplashPack, WallhavenPack,
};
use kitowall_backend::favorites::{add_favorite, list_favorites, load_favorites, remove_favorite};
use kitowall_backend::history::{append_history, clear_history, list_history, HistoryEntry};
use kitowall_backend::store::{load_config, load_state, save_config, save_state};
use kitowall_backend::{
    apply_next, apply_wallpaper, apply_wallpaper_batch, config_path, inspect_config, inspect_state,
    list_wallpapers, state_path, ApplyNextOptions, ApplyOutcome, ApplyWallpaperBatchOptions,
    ApplyWallpaperOptions, ApplyWallpaperTarget, CacheManager, ConfiguredProvider, JobKind,
    JobRecord, JobStatus, JobStore, LocalProvider, LogEntry, LogLevel, LogStore, ResolvedPool,
    StaticUrlProvider, UreqTransport, WallpaperApply, WallpaperGateway,
};
use serde::Serialize;

const CONTRACT_VERSION: &str = "1.0";
static LOCAL_MODE: OnceLock<bool> = OnceLock::new();

#[derive(Serialize)]
struct ContractMeta {
    cli: &'static str,
    cli_version: &'static str,
    contract_version: &'static str,
}

#[derive(Serialize)]
struct ContractSuccess<T: Serialize> {
    schema_version: u8,
    ok: bool,
    command: String,
    data: T,
    warnings: Vec<String>,
    meta: ContractMeta,
}

#[derive(Serialize)]
struct ContractFailure {
    schema_version: u8,
    ok: bool,
    command: String,
    error: ContractError,
    meta: ContractMeta,
}

#[derive(Serialize)]
struct ContractError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'static str>,
}

#[derive(Serialize)]
struct DoctorCheck {
    id: &'static str,
    ok: bool,
    severity: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<&'static str>,
}

#[derive(Serialize)]
struct AutomationIntent {
    schema_version: u8,
    id: String,
    description: String,
    command: Vec<String>,
    kind: String,
    restart: String,
    autostart: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule: Option<AutomationSchedule>,
}

#[derive(Serialize)]
struct AutomationBatchIntent {
    schema_version: u8,
    automations: Vec<AutomationIntent>,
}

#[derive(Serialize)]
struct AutomationSchedule {
    startup_delay_seconds: u64,
    every_seconds: u64,
}

#[derive(Debug, Serialize)]
struct StaticAutomationStatus {
    id: String,
    label: String,
    description: String,
    state: String,
    installed: bool,
    enabled: bool,
    active: bool,
    artifacts: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct CompositorGateway {
    binary: String,
    local: bool,
}

impl CompositorGateway {
    fn from_environment() -> Self {
        let local = local_mode();
        Self {
            binary: resolve_compositor_command(local),
            local,
        }
    }

    fn response(&self, args: &[String]) -> Result<(bool, serde_json::Value), String> {
        let mut args = args.to_vec();
        if self.local {
            args.push("--lc".into());
        }
        let output = Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|error| format!("failed to execute {}: {error}", self.binary))?;
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            format!(
                "invalid compositor response: {error}; stderr={}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
        let success =
            output.status.success() && value.get("ok") != Some(&serde_json::Value::Bool(false));
        Ok((success, value))
    }

    fn execute(&self, args: &[String]) -> Result<serde_json::Value, String> {
        let (success, value) = self.response(args)?;
        if !success {
            let message = value
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("compositor command failed");
            return Err(message.into());
        }
        Ok(value)
    }
}

impl WallpaperGateway for CompositorGateway {
    fn outputs(&self) -> Result<Vec<String>, String> {
        let value = self.execute(&["outputs".into(), "--contract-v1".into()])?;
        let outputs = value
            .pointer("/data/outputs")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "compositor response does not contain data.outputs".to_owned())?;
        outputs
            .iter()
            .map(|output| {
                output
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "compositor output does not contain a name".to_owned())
            })
            .collect()
    }

    fn apply(&self, request: &WallpaperApply) -> Result<(), String> {
        self.execute(&wallpaper_apply_args(request)).map(|_| ())
    }
}

fn wallpaper_apply_args(request: &WallpaperApply) -> Vec<String> {
    let mut args = vec![
        "wallpaper".into(),
        "apply".into(),
        "--namespace".into(),
        request.namespace.clone(),
        "--output".into(),
        request.output.clone(),
        "--image".into(),
        request.image.clone(),
        "--transition-type".into(),
        request.transition.kind.clone(),
        "--transition-fps".into(),
        request.transition.fps.to_string(),
        "--transition-duration".into(),
        request.transition.duration.to_string(),
    ];
    if let Some(angle) = request.transition.angle {
        args.extend(["--transition-angle".into(), angle.to_string()]);
    }
    if let Some(position) = &request.transition.pos {
        args.extend(["--transition-pos".into(), position.clone()]);
    }
    args.push("--contract-v1".into());
    args
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let local = cfg!(debug_assertions) || args.iter().any(|argument| argument == "--lc");
    args.retain(|argument| argument != "--lc");
    let _ = LOCAL_MODE.set(local);
    let contract = args.iter().any(|argument| argument == "--contract-v1");
    match run(args.clone()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if contract {
                let (code, exit, hint) = classify_error(&error);
                let _ = print_json(&ContractFailure {
                    schema_version: 1,
                    ok: false,
                    command: command_name(&args),
                    error: ContractError {
                        code,
                        message: error,
                        hint,
                    },
                    meta: contract_meta(),
                });
                ExitCode::from(exit)
            } else {
                eprintln!("kitowall: {error}");
                ExitCode::FAILURE
            }
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    match args.as_slice() {
        [command, rest @ ..]
            if (command == "version" || command == "--version") && frontend_flags_only(rest) =>
        {
            if contract_requested(rest) {
                emit_frontend_result(
                    "version",
                    serde_json::json!({"version": env!("CARGO_PKG_VERSION")}),
                    true,
                )?;
            } else {
                println!("kitowall {}", env!("CARGO_PKG_VERSION"));
            }
        }
        [command, rest @ ..] if command == "capabilities" && frontend_flags_only(rest) => {
            let contract = contract_requested(rest);
            emit_frontend_result(
                "capabilities",
                if contract {
                    capabilities_contract_data()
                } else {
                    capabilities_legacy_data()
                },
                contract,
            )?;
        }
        [group, command, rest @ ..]
            if group == "config" && command == "show" && frontend_flags_only(rest) =>
        {
            let config = load_config().map_err(|error| error.to_string())?;
            emit_frontend_result(
                "config show",
                sanitized_config(&config)?,
                contract_requested(rest),
            )?;
        }
        [group, command] if group == "config" && command == "init" => {
            load_config().map_err(|error| error.to_string())?;
            println!(
                "{}",
                config_path().map_err(|error| error.to_string())?.display()
            );
        }
        [command, rest @ ..] if command == "status" && frontend_flags_only(rest) => {
            let config = load_config().map_err(|error| error.to_string())?;
            let state = load_state().map_err(|error| error.to_string())?;
            let contract = contract_requested(rest);
            let data = if contract {
                serde_json::json!({
                    "product": "kitowall",
                    "mode": state.mode,
                    "current_pack": state.current_pack,
                    "outputs": state.last_outputs,
                    "last_set": state.last_set,
                    "last_updated_unix_ms": state.last_updated,
                    "configured_packs": config.packs.len(),
                    "config_path": config_path().map_err(|error| error.to_string())?,
                    "state_path": state_path().map_err(|error| error.to_string())?
                })
            } else {
                serde_json::json!({
                    "schemaVersion": 1,
                    "product": "kitowall",
                    "mode": state.mode,
                    "currentPack": state.current_pack,
                    "outputs": state.last_outputs,
                    "configuredPacks": config.packs.len(),
                    "configPath": config_path().map_err(|error| error.to_string())?,
                    "statePath": state_path().map_err(|error| error.to_string())?
                })
            };
            emit_frontend_result("status", data, contract)?;
        }
        [command, rest @ ..] if command == "outputs" && frontend_flags_only(rest) => {
            emit_frontend_result(
                "outputs",
                serde_json::json!({
                    "outputs": CompositorGateway::from_environment().outputs()?
                }),
                contract_requested(rest),
            )?;
        }
        [command, rest @ ..] if command == "doctor" && frontend_flags_only(rest) => {
            run_doctor(rest)?
        }
        [group, action, rest @ ..] if group == "settings" => run_settings(action, rest)?,
        [command, mode, rest @ ..] if command == "mode" && frontend_flags_only(rest) => {
            set_mode(mode, rest)?
        }
        [group, rest @ ..] if group == "transition" => set_transition(rest)?,
        [command, rest @ ..] if command == "favorites" && frontend_flags_only(rest) => {
            run_favorite("list", rest)?
        }
        [group, action, rest @ ..] if group == "favorite" => run_favorite(action, rest)?,
        [group, rest @ ..] if group == "history" => run_history(rest)?,
        [group, rest @ ..] if group == "logs" => run_logs(rest)?,
        [group, rest @ ..] if group == "job" => run_job(rest)?,
        [command, id] if command == "_job-worker" => run_job_worker(id)?,
        [group, action, rest @ ..] if group == "wallpaper" => run_wallpaper(action, rest)?,
        [group, action, rest @ ..] if group == "dashboard" => run_dashboard(action, rest)?,
        [command, rest @ ..] if command == "watch" => run_watch(rest)?,
        [group, action, rest @ ..] if group == "service" => run_service(action, rest)?,
        [command, rest @ ..] if command == "next" || command == "rotate-now" => {
            run_next(command, rest)?
        }
        [group, action, rest @ ..] if group == "pack" => run_pack(action, rest)?,
        [group, action, rest @ ..] if group == "cache" => run_cache(action, rest)?,
        [command] if command == "list-packs" => list_packs()?,
        _ => return Err(usage().into()),
    }
    Ok(())
}

fn run_dashboard(action: &str, args: &[String]) -> Result<(), String> {
    if action != "snapshot" || !frontend_flags_only_except(args, &["--pack"]) {
        return Err("usage: kitowall dashboard snapshot [--pack <name>]".into());
    }
    let contract = contract_requested(args);
    let config = load_config().map_err(|error| error.to_string())?;
    let state = load_state().map_err(|error| error.to_string())?;
    let favorites = load_favorites().map_err(|error| error.to_string())?;
    let requested_pack = option_value(args, "--pack");
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not available".to_owned())?;
    let catalog = list_wallpapers(&config, requested_pack, &home, &favorites, &state, 0, 200)?;
    let jobs = JobStore::from_environment()
        .map_err(|error| error.to_string())?
        .list()
        .map_err(|error| error.to_string())?;
    let history = list_history(None)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "timestamp": entry.timestamp,
                "pack": entry.pack,
                "output": entry.output,
                "path": entry.path,
                "favorite": favorites.contains(&entry.path)
            })
        })
        .collect::<Vec<_>>();
    let safe = sanitized_config(&config)?;
    let watch_paths = dashboard_watch_paths(&config, &home)?;
    let available = catalog.total;
    let downloaded = catalog.facets.hydrated;
    let local = catalog
        .facets
        .by_provider
        .get("local")
        .copied()
        .unwrap_or(0);
    let mut snapshot = serde_json::json!({
        "selectedPack": requested_pack,
        "catalog": catalog,
        "packs": safe["packs"],
        "providerCredentials": safe["providerCredentials"],
        "jobs": jobs,
        "history": history,
        "counts": {
            "available": available,
            "downloaded": downloaded,
            "local": local
        },
        "watchPaths": watch_paths
    });
    let bytes = serde_json::to_vec(&snapshot).map_err(|error| error.to_string())?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    snapshot["revision"] = serde_json::Value::String(format!("{:016x}", hasher.finish()));
    emit_frontend_result("dashboard snapshot", snapshot, contract)
}

fn dashboard_watch_paths(
    config: &kitowall_backend::Config,
    home: &Path,
) -> Result<Vec<String>, String> {
    let cache = CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
    let mut paths = std::collections::BTreeSet::new();
    for path in [
        config_path().map_err(|error| error.to_string())?,
        state_path().map_err(|error| error.to_string())?,
        cache.index_path().to_path_buf(),
    ] {
        paths.insert(
            path.parent()
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .into_owned(),
        );
    }
    paths.insert(cache.download_dir().to_string_lossy().into_owned());
    if let Some(indexes) = cache.index_path().parent().map(|path| path.join("indexes")) {
        paths.insert(indexes.to_string_lossy().into_owned());
    }
    for pack in config.packs.values() {
        if let PackConfig::Local { paths: local } = pack {
            for path in local {
                paths.insert(
                    expand_dashboard_path(path, home)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn expand_dashboard_path(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        home.to_path_buf()
    } else if let Some(relative) = path.strip_prefix("~/") {
        home.join(relative)
    } else {
        PathBuf::from(path)
    }
}

fn run_settings(action: &str, args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    match action {
        "get" if frontend_flags_only(args) => {
            let config = load_config().map_err(|error| error.to_string())?;
            emit_frontend_result(
                "settings get",
                serde_json::json!({
                    "mode": config.mode,
                    "rotation_interval_seconds": config.rotation_interval_seconds,
                    "wallpaper_backend": config.wallpaper_backend,
                    "transition": config.transition,
                    "selection": config.selection,
                    "cache": config.cache,
                    "pool": config.pool
                }),
                contract,
            )
        }
        "set" => {
            let interval = option_value(args, "--rotation-interval-seconds")
                .map(|value| parse_positive(value, "--rotation-interval-seconds"))
                .transpose()?
                .ok_or_else(|| {
                    "usage: kitowall settings set --rotation-interval-seconds <n>".to_owned()
                })?;
            let mut config = load_config().map_err(|error| error.to_string())?;
            config.rotation_interval_seconds = u64::from(interval);
            save_config(&config).map_err(|error| error.to_string())?;
            emit_frontend_result(
                "settings set",
                serde_json::json!({
                    "rotation_interval_seconds": config.rotation_interval_seconds
                }),
                contract,
            )
        }
        _ => Err("usage: kitowall settings <get|set --rotation-interval-seconds <n>>".into()),
    }
}

fn run_doctor(args: &[String]) -> Result<(), String> {
    let mut checks = Vec::new();
    checks.push(match inspect_config() {
        Ok(Some(_)) => DoctorCheck {
            id: "config",
            ok: true,
            severity: "info",
            message: "Kitowall configuration is valid".into(),
            remediation: None,
        },
        Ok(None) => DoctorCheck {
            id: "config",
            ok: false,
            severity: "warning",
            message: "Kitowall configuration has not been initialized".into(),
            remediation: Some("Run `kitowall config init`"),
        },
        Err(error) => DoctorCheck {
            id: "config",
            ok: false,
            severity: "error",
            message: error.to_string(),
            remediation: Some("Repair the Kitowall configuration before starting the frontend"),
        },
    });
    checks.push(match inspect_state() {
        Ok(Some(_)) => DoctorCheck {
            id: "state",
            ok: true,
            severity: "info",
            message: "Kitowall state is readable".into(),
            remediation: None,
        },
        Ok(None) => DoctorCheck {
            id: "state",
            ok: true,
            severity: "info",
            message: "Kitowall state will be created after the first mutation".into(),
            remediation: None,
        },
        Err(error) => DoctorCheck {
            id: "state",
            ok: false,
            severity: "error",
            message: error.to_string(),
            remediation: Some("Repair or restore the Kitowall state file"),
        },
    });

    let gateway = CompositorGateway::from_environment();
    checks.push(
        match gateway.execute(&["capabilities".into(), "--contract-v1".into()]) {
            Ok(_) => DoctorCheck {
                id: "compositor",
                ok: true,
                severity: "info",
                message: "Kitsune compositor contract is available".into(),
                remediation: None,
            },
            Err(error) => DoctorCheck {
                id: "compositor",
                ok: false,
                severity: "error",
                message: error,
                remediation: Some("Install or repair kitsune-compositor with GekkoApp"),
            },
        },
    );
    checks.push(match gateway.outputs() {
        Ok(outputs) if !outputs.is_empty() => DoctorCheck {
            id: "outputs",
            ok: true,
            severity: "info",
            message: format!("{} output(s) available", outputs.len()),
            remediation: None,
        },
        Ok(_) => DoctorCheck {
            id: "outputs",
            ok: false,
            severity: "warning",
            message: "No graphical outputs are currently available".into(),
            remediation: Some("Start a supported graphical session and retry"),
        },
        Err(error) => DoctorCheck {
            id: "outputs",
            ok: false,
            severity: "warning",
            message: error,
            remediation: Some("Check the compositor session and output adapter"),
        },
    });

    let automation_ids = [
        "kitowall-runtime",
        "kitowall-next",
        "kitowall-watch",
        "kitowall-login-apply",
    ];
    let available = automation_ids
        .iter()
        .filter(|id| {
            gateway
                .execute(&[
                    "automation".into(),
                    "status".into(),
                    "--id".into(),
                    (**id).into(),
                    "--contract-v1".into(),
                ])
                .is_ok()
        })
        .count();
    checks.push(DoctorCheck {
        id: "automations",
        ok: available == automation_ids.len(),
        severity: if available == automation_ids.len() {
            "info"
        } else {
            "warning"
        },
        message: format!(
            "{available}/{} automations registered",
            automation_ids.len()
        ),
        remediation: (available != automation_ids.len())
            .then_some("Run `kitowall service plan` and `kitowall service apply`"),
    });
    let healthy = checks
        .iter()
        .all(|check| check.ok || check.severity != "error");
    emit_frontend_result(
        "doctor",
        serde_json::json!({"healthy": healthy, "checks": checks}),
        contract_requested(args),
    )
}

fn run_wallpaper(action: &str, args: &[String]) -> Result<(), String> {
    let contract = args.iter().any(|argument| argument == "--contract-v1");
    match action {
        "list" => {
            let offset = option_value(args, "--offset")
                .map(|value| parse_nonnegative(value, "--offset"))
                .transpose()?
                .unwrap_or(0);
            let limit = option_value(args, "--limit")
                .map(|value| parse_positive(value, "--limit"))
                .transpose()?
                .unwrap_or(50) as usize;
            let config = load_config().map_err(|error| error.to_string())?;
            let state = load_state().map_err(|error| error.to_string())?;
            let favorites = load_favorites().map_err(|error| error.to_string())?;
            let home = env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "HOME is not available".to_owned())?;
            let page = list_wallpapers(
                &config,
                option_value(args, "--pack"),
                &home,
                &favorites,
                &state,
                offset,
                limit,
            )?;
            emit_frontend_result("wallpaper list", page, contract)
        }
        "apply" => {
            let pack = required_cli_option(args, "--pack")?;
            let id = required_cli_option(args, "--id")?;
            let output = required_cli_option(args, "--output")?;
            let namespace = option_value(args, "--namespace").unwrap_or("kitowall");
            let now_ms = current_time_ms()?;
            let config = load_config().map_err(|error| error.to_string())?;
            let mut state = load_state().map_err(|error| error.to_string())?;
            let home = env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "HOME is not available".to_owned())?;
            let transport = UreqTransport::default();
            let pool = ResolvedPool::resolve(
                &config,
                Some(pack),
                state.current_pack.as_deref(),
                &home,
                &transport,
                now_ms,
            )?;
            let result = apply_wallpaper(
                &config,
                &mut state,
                &pool,
                &transport,
                &CompositorGateway::from_environment(),
                &ApplyWallpaperOptions {
                    id: id.into(),
                    output: output.into(),
                    namespace: namespace.into(),
                    now_ms,
                },
            )?;
            save_state(&state).map_err(|error| error.to_string())?;
            append_history(&[HistoryEntry {
                timestamp: now_ms,
                pack: result.pack.clone(),
                output: result.output.clone(),
                path: result.path.clone(),
            }])
            .map_err(|error| error.to_string())?;
            record_log(LogEntry {
                timestamp_unix_ms: now_ms,
                level: LogLevel::Info,
                source: "wallpaper".into(),
                action: "apply".into(),
                message: "Wallpaper applied successfully".into(),
                pack: Some(result.pack.clone()),
                output: Some(result.output.clone()),
                path: Some(result.path.clone()),
            });
            emit_frontend_result("wallpaper apply", result, contract)
        }
        "apply-batch" => {
            let pack = required_cli_option(args, "--pack")?;
            let targets = parse_wallpaper_map(required_cli_option(args, "--map")?)?;
            let namespace = option_value(args, "--namespace").unwrap_or("kitowall");
            let now_ms = current_time_ms()?;
            let config = load_config().map_err(|error| error.to_string())?;
            let mut state = load_state().map_err(|error| error.to_string())?;
            let home = env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "HOME is not available".to_owned())?;
            let transport = UreqTransport::default();
            let pool = ResolvedPool::resolve(
                &config,
                Some(pack),
                state.current_pack.as_deref(),
                &home,
                &transport,
                now_ms,
            )?;
            let result = apply_wallpaper_batch(
                &config,
                &mut state,
                &pool,
                &transport,
                &CompositorGateway::from_environment(),
                &ApplyWallpaperBatchOptions {
                    targets,
                    namespace: namespace.into(),
                    now_ms,
                },
            )?;
            save_state(&state).map_err(|error| error.to_string())?;
            let entries = result
                .images
                .iter()
                .map(|image| HistoryEntry {
                    timestamp: now_ms,
                    pack: image.pack.clone(),
                    output: image.output.clone(),
                    path: image.path.clone(),
                })
                .collect::<Vec<_>>();
            append_history(&entries).map_err(|error| error.to_string())?;
            record_log(LogEntry {
                timestamp_unix_ms: now_ms,
                level: LogLevel::Info,
                source: "wallpaper".into(),
                action: "apply_batch".into(),
                message: format!("Applied {} wallpaper(s)", result.images.len()),
                pack: Some(result.pack.clone()),
                output: None,
                path: None,
            });
            emit_frontend_result("wallpaper apply-batch", result, contract)
        }
        _ => Err(
            "usage: kitowall wallpaper <list [--pack <name>] [--offset <n>] [--limit <n>]|apply --pack <name> --id <id> --output <name>|apply-batch --pack <name> --map <output:id,...>>".into(),
        ),
    }
}

fn run_watch(args: &[String]) -> Result<(), String> {
    let poll_ms = option_value(args, "--poll-ms")
        .map(|value| parse_positive(value, "--poll-ms"))
        .transpose()?
        .unwrap_or(1000) as u64;
    if !(100..=60_000).contains(&poll_ms) {
        return Err("--poll-ms must be between 100 and 60000".into());
    }
    let once = args.iter().any(|argument| argument == "--once");
    let namespace = option_value(args, "--namespace").unwrap_or("kitowall");
    let gateway = CompositorGateway::from_environment();
    let mut previous = Vec::new();
    loop {
        let outputs = gateway.outputs()?;
        if outputs != previous {
            let next_args = vec![
                "--force".to_owned(),
                "--namespace".to_owned(),
                namespace.to_owned(),
            ];
            run_next("next", &next_args)?;
            previous = outputs;
        }
        if once {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(poll_ms));
    }
}

fn run_service(action: &str, args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    const AUTOMATIONS: &[(&str, &str, &str)] = &[
        (
            "kitowall-runtime",
            "Runtime estatico",
            "Mantiene disponible el backend awww o swww.",
        ),
        (
            "kitowall-next",
            "Rotacion programada",
            "Selecciona y aplica el siguiente wallpaper con el intervalo configurado.",
        ),
        (
            "kitowall-watch",
            "Cambios de monitores",
            "Detecta cambios de outputs y vuelve a aplicar la configuracion.",
        ),
        (
            "kitowall-login-apply",
            "Restauracion de sesion",
            "Restaura los wallpapers al iniciar la sesion grafica.",
        ),
    ];
    if action == "status" {
        let gateway = CompositorGateway::from_environment();
        let automations = AUTOMATIONS
            .iter()
            .map(|(id, label, description)| {
                let response = gateway.response(&[
                    "automation".into(),
                    "status".into(),
                    "--id".into(),
                    (*id).into(),
                    "--contract-v1".into(),
                ])?;
                Ok::<StaticAutomationStatus, String>(normalize_automation_status(
                    id,
                    label,
                    description,
                    response.0,
                    &response.1,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let installed = automations.iter().filter(|status| status.installed).count();
        let enabled = automations.iter().filter(|status| status.enabled).count();
        let active = automations.iter().filter(|status| status.active).count();
        let errors = automations
            .iter()
            .filter(|status| status.state == "error")
            .count();
        return emit_frontend_result(
            "service status",
            serde_json::json!({
                "action": "status",
                "summary": {
                    "total": automations.len(),
                    "installed": installed,
                    "enabled": enabled,
                    "active": active,
                    "errors": errors,
                    "healthy": installed == automations.len()
                        && enabled == automations.len()
                        && errors == 0
                },
                "automations": automations
            }),
            contract,
        );
    }
    if matches!(
        action,
        "start" | "stop" | "restart" | "enable" | "disable" | "remove"
    ) {
        let gateway = CompositorGateway::from_environment();
        let mut ids = AUTOMATIONS.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
        if matches!(action, "stop" | "disable" | "remove") {
            ids.reverse();
        }
        let mut results = Vec::new();
        for id in ids {
            results.push(gateway.execute(&[
                "automation".into(),
                action.into(),
                "--id".into(),
                id.into(),
                "--contract-v1".into(),
            ])?);
        }
        let data = if contract {
            serde_json::json!({"action": action, "automations": results})
        } else {
            serde_json::json!({"ok": true, "action": action, "automations": results})
        };
        return emit_frontend_result(&format!("service {action}"), data, contract);
    }
    if action == "reschedule" {
        let every_seconds = option_value(args, "--every-seconds")
            .map(|value| parse_positive(value, "--every-seconds"))
            .transpose()?
            .map(u64::from)
            .unwrap_or_else(|| {
                load_config()
                    .map(|config| config.rotation_interval_seconds)
                    .unwrap_or(1800)
            });
        let namespace = option_value(args, "--namespace").unwrap_or("kitowall");
        let compositor = resolve_executable(&resolve_compositor_command(local_mode()))?;
        let gateway = CompositorGateway {
            binary: compositor.to_string_lossy().into_owned(),
            local: local_mode(),
        };
        let status = gateway.response(&[
            "automation".into(),
            "status".into(),
            "--id".into(),
            "kitowall-next".into(),
            "--contract-v1".into(),
        ])?;
        let artifacts = status
            .1
            .pointer("/data/artifacts")
            .and_then(serde_json::Value::as_array);
        let installed = status.0
            && artifacts.is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("installed").and_then(serde_json::Value::as_bool) == Some(true)
                })
            });
        let enabled = artifacts.is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("enabled").and_then(serde_json::Value::as_bool) == Some(true))
        });
        if !installed {
            return emit_frontend_result(
                "service reschedule",
                serde_json::json!({
                    "action": "reschedule",
                    "id": "kitowall-next",
                    "every_seconds": every_seconds,
                    "materialized": false,
                    "enabled": false
                }),
                contract,
            );
        }

        let kitowall = env::current_exe()
            .map_err(|error| format!("failed to resolve kitowall executable: {error}"))?;
        let mut intents = automation_intents(
            &kitowall.to_string_lossy(),
            &compositor.to_string_lossy(),
            namespace,
            every_seconds,
        );
        let rotation = intents.remove(1);
        let request = AutomationBatchIntent {
            schema_version: 1,
            automations: vec![rotation],
        };
        let path = write_automation_batch_request(&request)?;
        let result = gateway.execute(&[
            "automation".into(),
            "apply-batch".into(),
            "--descriptor".into(),
            path.to_string_lossy().into_owned(),
            "--contract-v1".into(),
        ]);
        let _ = std::fs::remove_file(&path);
        let batch = result?;
        if enabled {
            gateway.execute(&[
                "automation".into(),
                "restart".into(),
                "--id".into(),
                "kitowall-next".into(),
                "--contract-v1".into(),
            ])?;
        }
        return emit_frontend_result(
            "service reschedule",
            serde_json::json!({
                "action": "reschedule",
                "id": "kitowall-next",
                "every_seconds": every_seconds,
                "materialized": true,
                "enabled": enabled,
                "batch": batch
            }),
            contract,
        );
    }
    if !matches!(action, "plan" | "apply") {
        return Err(
            "usage: kitowall service <plan|apply|reschedule|status|start|stop|restart|enable|disable|remove> [options]".into(),
        );
    }
    let every_seconds = option_value(args, "--every-seconds")
        .map(|value| parse_positive(value, "--every-seconds"))
        .transpose()?
        .map(u64::from)
        .unwrap_or_else(|| {
            load_config()
                .map(|config| config.rotation_interval_seconds)
                .unwrap_or(1800)
        });
    let namespace = option_value(args, "--namespace").unwrap_or("kitowall");
    let kitowall = env::current_exe()
        .map_err(|error| format!("failed to resolve kitowall executable: {error}"))?;
    let compositor = resolve_executable(&resolve_compositor_command(local_mode()))?;
    let intents = automation_intents(
        &kitowall.to_string_lossy(),
        &compositor.to_string_lossy(),
        namespace,
        every_seconds,
    );
    let gateway = CompositorGateway {
        binary: compositor.to_string_lossy().into_owned(),
        local: local_mode(),
    };
    let request = AutomationBatchIntent {
        schema_version: 1,
        automations: intents,
    };
    let path = write_automation_batch_request(&request)?;
    let compositor_action = if action == "plan" {
        "plan-batch"
    } else {
        "apply-batch"
    };
    let result = gateway.execute(&[
        "automation".into(),
        compositor_action.into(),
        "--descriptor".into(),
        path.to_string_lossy().into_owned(),
        "--contract-v1".into(),
    ]);
    let _ = std::fs::remove_file(&path);
    let batch = result?;
    let hint = (action == "apply")
        .then_some("run `kitowall service enable` after reviewing the applied artifacts");
    let data = if contract {
        serde_json::json!({
            "action": action,
            "batch": batch,
            "activation_required": action == "apply",
            "activation_hint": hint
        })
    } else {
        serde_json::json!({
            "ok": true,
            "action": action,
            "batch": batch,
            "activationRequired": action == "apply",
            "activationHint": hint
        })
    };
    emit_frontend_result(&format!("service {action}"), data, contract)
}

fn normalize_automation_status(
    id: &str,
    label: &str,
    description: &str,
    success: bool,
    response: &serde_json::Value,
) -> StaticAutomationStatus {
    let artifacts = response
        .pointer("/data/artifacts")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let installed = success
        && !artifacts.is_empty()
        && artifacts
            .iter()
            .all(|artifact| artifact["installed"].as_bool() == Some(true));
    let enabled = installed
        && artifacts
            .iter()
            .any(|artifact| artifact["enabled"].as_bool() == Some(true));
    let active = installed
        && artifacts
            .iter()
            .any(|artifact| artifact["active"].as_bool() == Some(true));
    let error = (!success).then(|| {
        response
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("automation status is unavailable")
            .to_owned()
    });
    let missing = error
        .as_deref()
        .is_some_and(|message| message.starts_with("automation id not found:"));
    let state = if missing {
        "not_installed"
    } else if !success {
        "error"
    } else if active {
        "active"
    } else if enabled {
        "enabled"
    } else if installed {
        "stopped"
    } else {
        "not_installed"
    };
    StaticAutomationStatus {
        id: id.into(),
        label: label.into(),
        description: description.into(),
        state: state.into(),
        installed,
        enabled,
        active,
        artifacts,
        error,
    }
}

fn automation_intents(
    kitowall: &str,
    compositor: &str,
    namespace: &str,
    every_seconds: u64,
) -> Vec<AutomationIntent> {
    vec![
        AutomationIntent {
            schema_version: 1,
            id: "kitowall-runtime".into(),
            description: "Ensure the static wallpaper runtime is available".into(),
            command: vec![
                compositor.into(),
                "wallpaper".into(),
                "start".into(),
                "--namespace".into(),
                namespace.into(),
                "--contract-v1".into(),
            ],
            kind: "one_shot".into(),
            restart: "no".into(),
            autostart: true,
            schedule: None,
        },
        AutomationIntent {
            schema_version: 1,
            id: "kitowall-next".into(),
            description: "Rotate Kitowall static wallpapers".into(),
            command: vec![
                kitowall.into(),
                "rotate-now".into(),
                "--namespace".into(),
                namespace.into(),
            ],
            kind: "one_shot".into(),
            restart: "no".into(),
            autostart: false,
            schedule: Some(AutomationSchedule {
                startup_delay_seconds: 2,
                every_seconds,
            }),
        },
        AutomationIntent {
            schema_version: 1,
            id: "kitowall-watch".into(),
            description: "Watch output changes and reapply Kitowall".into(),
            command: vec![
                kitowall.into(),
                "watch".into(),
                "--namespace".into(),
                namespace.into(),
            ],
            kind: "daemon".into(),
            restart: "on-failure".into(),
            autostart: true,
            schedule: None,
        },
        AutomationIntent {
            schema_version: 1,
            id: "kitowall-login-apply".into(),
            description: "Restore Kitowall when the graphical session starts".into(),
            command: vec![
                kitowall.into(),
                "rotate-now".into(),
                "--namespace".into(),
                namespace.into(),
            ],
            kind: "one_shot".into(),
            restart: "no".into(),
            autostart: true,
            schedule: None,
        },
    ]
}

fn resolve_executable(value: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(value);
    if path.is_absolute() && path.is_file() {
        return Ok(path);
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(value))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("executable not found: {value}"))
}

fn local_mode() -> bool {
    LOCAL_MODE.get().copied().unwrap_or(false)
}

fn resolve_compositor_command(local: bool) -> String {
    if let Ok(binary) = env::var("KITSUNE_COMPOSITOR_BIN") {
        return binary;
    }
    if local {
        if let Some(binary) = local_project_binary("compositor", "kitsune-compositor") {
            return binary.to_string_lossy().into_owned();
        }
    }
    "kitsune-compositor".into()
}

fn local_project_binary(project: &str, binary: &str) -> Option<PathBuf> {
    let root = env::current_dir()
        .ok()
        .and_then(|path| find_refactor_root(&path))
        .or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|path| find_refactor_root(&path))
        })?;
    let preferred = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .and_then(|path| path.file_name().map(|name| name.to_owned()))
        .and_then(|name| name.to_str().map(str::to_owned))
        .filter(|profile| matches!(profile.as_str(), "debug" | "release"))
        .unwrap_or_else(|| "debug".into());
    let resolved = [preferred.as_str(), "debug", "release"]
        .into_iter()
        .map(|profile| root.join(project).join("target").join(profile).join(binary))
        .find(|candidate| candidate.is_file());
    resolved
}

fn find_refactor_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|candidate| {
        (candidate.join("kitowall/Cargo.toml").is_file()
            && candidate.join("compositor/Cargo.toml").is_file()
            && candidate.join("kiui/Cargo.toml").is_file())
        .then(|| candidate.to_path_buf())
    })
}

fn write_automation_batch_request(
    request: &AutomationBatchIntent,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "kitowall-automation-batch-{}-{nonce}.json",
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(request).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(path)
}

fn run_favorite(action: &str, args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let values = args
        .iter()
        .filter(|argument| !matches!(argument.as_str(), "--contract-v1" | "--json"))
        .collect::<Vec<_>>();
    match action {
        "list" if values.is_empty() => emit_frontend_result(
            "favorite list",
            serde_json::json!({
                "favorites": list_favorites().map_err(|error| error.to_string())?
            }),
            contract,
        ),
        "add" | "remove" if values.len() == 1 => {
            let path = non_empty(values[0], "path")?;
            let changed = if action == "add" {
                add_favorite(&path)
            } else {
                remove_favorite(&path)
            }
            .map_err(|error| error.to_string())?;
            emit_frontend_result(
                &format!("favorite {action}"),
                serde_json::json!({
                    "action": action,
                    "path": path,
                    "changed": changed
                }),
                contract,
            )
        }
        _ => Err("usage: kitowall favorite <list|add <path>|remove <path>>".into()),
    }
}

fn run_history(args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let args = args
        .iter()
        .filter(|argument| !matches!(argument.as_str(), "--contract-v1" | "--json"))
        .cloned()
        .collect::<Vec<_>>();
    if args.first().is_some_and(|value| value == "clear") {
        if args.len() != 1 {
            return Err("usage: kitowall history clear".into());
        }
        let removed = clear_history().map_err(|error| error.to_string())?;
        return emit_frontend_result(
            "history clear",
            serde_json::json!({"removed": removed}),
            contract,
        );
    }
    let args = if args.first().is_some_and(|value| value == "list") {
        &args[1..]
    } else {
        &args
    };
    if !args.is_empty() && (args.len() != 2 || args[0] != "--limit") {
        return Err("usage: kitowall history [list] [--limit <n>]".into());
    }
    let limit = option_value(args, "--limit")
        .map(|value| parse_positive(value, "--limit"))
        .transpose()?
        .map(|value| value as usize);
    let favorites = load_favorites().map_err(|error| error.to_string())?;
    let entries = list_history(limit)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "timestamp": entry.timestamp,
                "pack": entry.pack,
                "output": entry.output,
                "path": entry.path,
                "favorite": favorites.contains(&entry.path)
            })
        })
        .collect::<Vec<_>>();
    emit_frontend_result(
        "history list",
        serde_json::json!({"entries": entries}),
        contract,
    )
}

fn run_logs(args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let action = args
        .iter()
        .find(|argument| !matches!(argument.as_str(), "--contract-v1" | "--json"))
        .map(String::as_str)
        .unwrap_or("list");
    let store = LogStore::from_environment().map_err(|error| error.to_string())?;
    match action {
        "list" => {
            let limit = option_value(args, "--limit")
                .map(|value| parse_positive(value, "--limit"))
                .transpose()?
                .unwrap_or(100) as usize;
            if limit > 1_000 {
                return Err("--limit cannot exceed 1000".into());
            }
            let level = option_value(args, "--level")
                .map(parse_log_level)
                .transpose()?;
            let entries = store
                .list(
                    limit,
                    level,
                    option_value(args, "--source"),
                    option_value(args, "--pack"),
                )
                .map_err(|error| error.to_string())?;
            emit_frontend_result("logs list", serde_json::json!({"entries": entries}), contract)
        }
        "clear" => {
            let removed = store.clear().map_err(|error| error.to_string())?;
            emit_frontend_result("logs clear", serde_json::json!({"removed": removed}), contract)
        }
        _ => Err(
            "usage: kitowall logs [list] [--limit <n>] [--level <info|warning|error>] [--source <name>] [--pack <name>] | logs clear".into(),
        ),
    }
}

fn parse_log_level(value: &str) -> Result<LogLevel, String> {
    match value {
        "info" => Ok(LogLevel::Info),
        "warning" | "warn" => Ok(LogLevel::Warning),
        "error" => Ok(LogLevel::Error),
        _ => Err(format!("invalid --level: {value}")),
    }
}

fn record_log(entry: LogEntry) {
    let _ = LogStore::from_environment().and_then(|store| store.append(entry));
}

fn set_mode(value: &str, args: &[String]) -> Result<(), String> {
    let mode = match value {
        "manual" => Mode::Manual,
        "rotate" => Mode::Rotate,
        _ => return Err("usage: kitowall mode <manual|rotate>".into()),
    };
    let now_ms = current_time_ms()?;
    let mut config = load_config().map_err(|error| error.to_string())?;
    let mut state = load_state().map_err(|error| error.to_string())?;
    config.mode = mode;
    state.set_mode(mode, now_ms);
    save_config(&config).map_err(|error| error.to_string())?;
    save_state(&state).map_err(|error| error.to_string())?;
    let contract = contract_requested(args);
    emit_frontend_result(
        "mode",
        if contract {
            serde_json::json!({"mode": mode})
        } else {
            serde_json::json!({"ok": true, "mode": mode})
        },
        contract,
    )
}

fn set_transition(args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let args = if args.first().is_some_and(|value| value == "set") {
        &args[1..]
    } else {
        args
    };
    if args.is_empty() {
        return Err("usage: kitowall transition set [--enabled <bool>] [--type <type>] [--fps <1-240>] [--duration <0-60>] [--angle <n>] [--pos <x,y>]".into());
    }
    let mut config = load_config().map_err(|error| error.to_string())?;
    if let Some(value) = option_value(args, "--enabled") {
        if parse_bool(value, "--enabled")? {
            if config.transition.duration == 0.0 {
                config.transition.duration = 0.7;
            }
        } else {
            config.transition.duration = 0.0;
        }
    }
    if let Some(value) = option_value(args, "--type") {
        let kind = value.trim().to_ascii_lowercase();
        if !is_supported_transition(&kind) {
            return Err(format!("invalid --type: {value}"));
        }
        config.transition.kind = kind;
    }
    if let Some(value) = option_value(args, "--fps") {
        let fps = value
            .parse::<u32>()
            .ok()
            .filter(|fps| (1..=240).contains(fps))
            .ok_or_else(|| format!("invalid --fps: {value}"))?;
        config.transition.fps = fps;
    }
    if let Some(value) = option_value(args, "--duration") {
        config.transition.duration = parse_range_f64(value, "--duration", 0.0, 60.0)?;
    }
    if let Some(value) = option_value(args, "--angle") {
        config.transition.angle = Some(parse_range_f64(value, "--angle", -360.0, 360.0)?);
    }
    if let Some(value) = option_value(args, "--pos") {
        config.transition.pos = Some(non_empty(value, "--pos")?);
    }
    save_config(&config).map_err(|error| error.to_string())?;
    emit_frontend_result(
        "transition set",
        if contract {
            serde_json::json!({
                "animated": config.transition.duration > 0.0,
                "transition": config.transition
            })
        } else {
            serde_json::json!({
                "ok": true,
                "animated": config.transition.duration > 0.0,
                "transition": config.transition
            })
        },
        contract,
    )
}

fn run_next(command: &str, args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let force = command == "rotate-now" || args.iter().any(|argument| argument == "--force");
    let namespace = option_value(args, "--namespace").unwrap_or("kitowall");
    let requested_pack = option_value(args, "--pack");
    let config = load_config().map_err(|error| error.to_string())?;
    let mut state = load_state().map_err(|error| error.to_string())?;
    if state.mode == Mode::Manual && !force {
        return emit_frontend_result(
            command,
            serde_json::json!({
                "outcome": "skipped",
                "reason": "mode=manual",
                "hint": "use kitowall rotate-now, kitowall next --force, or kitowall mode rotate"
            }),
            contract,
        );
    }
    let home = env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "HOME is not available".to_owned())?;
    let now_ms = current_time_ms()?;
    let transport = UreqTransport::default();
    let pool = ResolvedPool::resolve(
        &config,
        requested_pack,
        state.current_pack.as_deref(),
        Path::new(&home),
        &transport,
        now_ms,
    )?;
    let gateway = CompositorGateway::from_environment();
    let outcome = apply_next(
        &config,
        &mut state,
        &pool,
        &transport,
        &gateway,
        &ApplyNextOptions {
            force,
            namespace: namespace.into(),
            now_ms,
            start_offset: now_ms as usize % pool.paths.len().max(1),
        },
    )?;
    if let ApplyOutcome::Applied(result) = &outcome {
        save_state(&state).map_err(|error| error.to_string())?;
        let entries = result
            .images
            .iter()
            .map(|image| HistoryEntry {
                timestamp: now_ms,
                pack: result.pack.clone(),
                output: image.output.clone(),
                path: image.path.clone(),
            })
            .collect::<Vec<_>>();
        append_history(&entries).map_err(|error| error.to_string())?;
        record_log(LogEntry {
            timestamp_unix_ms: now_ms,
            level: LogLevel::Info,
            source: "rotation".into(),
            action: command.into(),
            message: format!("Applied {} wallpaper(s)", result.images.len()),
            pack: Some(result.pack.clone()),
            output: None,
            path: None,
        });
    }
    emit_frontend_result(command, outcome, contract)
}

fn run_cache(action: &str, args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let args = args
        .iter()
        .filter(|argument| !matches!(argument.as_str(), "--contract-v1" | "--json"))
        .cloned()
        .collect::<Vec<_>>();
    let config = load_config().map_err(|error| error.to_string())?;
    let manager = CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
    let index = manager.load_index().map_err(|error| error.to_string())?;
    let favorites = load_favorites().map_err(|error| error.to_string())?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis() as u64;

    match action {
        "status" if args.is_empty() => emit_frontend_result(
            "cache status",
            manager.status(&index, &favorites, now_ms),
            contract,
        ),
        "plan" => {
            if !args.is_empty() && (args.len() != 2 || args[0] != "--pack") {
                return Err("usage: kitowall cache plan [--pack <name>]".into());
            }
            let pack = option_value(&args, "--pack").map(normalize_pack_name);
            if pack.as_deref() == Some("") {
                return Err("--pack must contain a valid pack name".into());
            }
            emit_frontend_result(
                "cache plan",
                manager.plan_prune(&index, &favorites, now_ms, pack.as_deref()),
                contract,
            )
        }
        "prune" => {
            let confirm = args.iter().any(|argument| argument == "--confirm");
            let filtered = args
                .iter()
                .filter(|argument| argument.as_str() != "--confirm")
                .cloned()
                .collect::<Vec<_>>();
            if !filtered.is_empty() && (filtered.len() != 2 || filtered[0] != "--pack") {
                return Err("usage: kitowall cache prune [--pack <name>] --confirm".into());
            }
            if !confirm {
                return Err("cache prune requires --confirm; inspect cache plan first".into());
            }
            let pack = option_value(&filtered, "--pack").map(normalize_pack_name);
            if pack.as_deref() == Some("") {
                return Err("--pack must contain a valid pack name".into());
            }
            let plan = manager.plan_prune(&index, &favorites, now_ms, pack.as_deref());
            let result = manager
                .apply_prune(&index, &plan)
                .map_err(|error| error.to_string())?;
            record_log(LogEntry {
                timestamp_unix_ms: now_ms,
                level: if result.cleanup_failures.is_empty() {
                    LogLevel::Info
                } else {
                    LogLevel::Warning
                },
                source: "cache".into(),
                action: "cache prune".into(),
                message: format!(
                    "Removed {} cache entries and {} files",
                    result.removed_entries, result.removed_files
                ),
                pack: result.pack.clone(),
                output: None,
                path: None,
            });
            emit_frontend_result("cache prune", result, contract)
        }
        _ => Err(
            "usage: kitowall cache <status|plan [--pack <name>]|prune [--pack <name>] --confirm>"
                .into(),
        ),
    }
}

fn run_job(args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    let values = args
        .iter()
        .filter(|argument| !matches!(argument.as_str(), "--contract-v1" | "--json"))
        .cloned()
        .collect::<Vec<_>>();
    let store = JobStore::from_environment().map_err(|error| error.to_string())?;
    match values.as_slice() {
        [action] if action == "list" => emit_frontend_result(
            "job list",
            serde_json::json!({"jobs": store.list().map_err(|error| error.to_string())?}),
            contract,
        ),
        [action, id] if action == "status" => emit_frontend_result(
            "job status",
            store.load(id).map_err(|error| error.to_string())?,
            contract,
        ),
        [action, id] if action == "cancel" => emit_frontend_result(
            "job cancel",
            store
                .request_cancel(id)
                .map_err(|error| error.to_string())?,
            contract,
        ),
        [action, kind, pack, rest @ ..] if action == "start" => {
            let kind = match kind.as_str() {
                "refresh" => JobKind::Refresh,
                "hydrate" => JobKind::Hydrate,
                _ => return Err("job kind must be refresh or hydrate".into()),
            };
            let pack = normalize_pack_name(pack);
            if pack.is_empty() {
                return Err("pack name cannot be empty".into());
            }
            let config = load_config().map_err(|error| error.to_string())?;
            let configured = config
                .packs
                .get(&pack)
                .ok_or_else(|| format!("pack not found: {pack}"))?;
            if kind == JobKind::Hydrate && matches!(configured, PackConfig::Local { .. }) {
                return Err("local packs do not require hydration".into());
            }
            let total = if kind == JobKind::Hydrate {
                hydration_count(rest)?
            } else {
                if !rest.is_empty() {
                    return Err("refresh jobs do not accept additional options".into());
                }
                1
            };
            let record = store
                .create(kind, pack, total)
                .map_err(|error| error.to_string())?;
            let executable = env::current_exe().map_err(|error| error.to_string())?;
            if let Err(error) = Command::new(executable)
                .args(["_job-worker", &record.id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                let mut failed = record.clone();
                failed.status = JobStatus::Failed;
                failed.error = Some(format!("could not start worker: {error}"));
                failed.updated_at_unix_ms = current_time_ms()?;
                store.save(&failed).map_err(|error| error.to_string())?;
                return Err(failed.error.unwrap());
            }
            emit_frontend_result("job start", record, contract)
        }
        _ => Err(
            "usage: kitowall job <list|status <id>|cancel <id>|start <refresh|hydrate> <pack> [--count <n>]>"
                .into(),
        ),
    }
}

fn run_job_worker(id: &str) -> Result<(), String> {
    let store = JobStore::from_environment().map_err(|error| error.to_string())?;
    let mut record = store.load(id).map_err(|error| error.to_string())?;
    if record.status == JobStatus::Canceled {
        return Ok(());
    }
    record.status = JobStatus::Running;
    record.updated_at_unix_ms = current_time_ms()?;
    store.save(&record).map_err(|error| error.to_string())?;

    let result = match record.kind {
        JobKind::Refresh => run_pack("refresh", &[record.pack.clone()]),
        JobKind::Hydrate => run_hydration_job(&store, &record),
    };
    let mut latest = store.load(id).map_err(|error| error.to_string())?;
    if latest.status == JobStatus::Canceled {
        // The hydration loop already persisted the terminal cancellation state.
    } else if latest.status == JobStatus::CancelRequested {
        latest.status = JobStatus::Canceled;
    } else if let Err(error) = result {
        latest.status = JobStatus::Failed;
        latest.error = Some(error);
    } else {
        latest.status = JobStatus::Completed;
        if latest.kind == JobKind::Refresh {
            latest.completed = latest.total;
        }
    }
    latest.updated_at_unix_ms = current_time_ms()?;
    store.save(&latest).map_err(|error| error.to_string())?;
    Ok(())
}

fn run_hydration_job(store: &JobStore, record: &JobRecord) -> Result<(), String> {
    let config = load_config().map_err(|error| error.to_string())?;
    let pack = config
        .packs
        .get(&record.pack)
        .ok_or_else(|| format!("pack not found: {}", record.pack))?;
    match pack {
        PackConfig::StaticUrl(pack) => {
            let provider = static_url_provider(&record.pack, pack.clone(), &config)?;
            let transport = UreqTransport::default();
            let mut candidates = provider
                .list_candidates()
                .map_err(|error| error.to_string())?;
            if candidates.is_empty() {
                provider
                    .refresh_index(current_time_ms()?)
                    .map_err(|error| error.to_string())?;
                candidates = provider
                    .list_candidates()
                    .map_err(|error| error.to_string())?;
            }
            if candidates.is_empty() {
                return Err("pack refresh completed without candidates".into());
            }
            retain_pending_candidates(&mut candidates, record.total, |candidate| {
                provider.local_path_for(candidate)
            });
            set_hydration_job_total(store, record, candidates.len())?;
            for candidate in &candidates {
                if advance_job_or_cancel(store, record)? {
                    return Ok(());
                }
                provider
                    .hydrate(candidate, &transport, current_time_ms()?)
                    .map_err(|error| error.to_string())?;
                increment_job(store, record)?;
            }
        }
        PackConfig::Local { .. } => return Err("local packs do not require hydration".into()),
        _ => {
            let provider = configured_provider(&record.pack, pack, &config)?;
            let transport = UreqTransport::default();
            let mut candidates = provider.list().map_err(|error| error.to_string())?;
            if candidates.is_empty() {
                provider
                    .refresh(&transport, current_time_ms()?)
                    .map_err(|error| error.to_string())?;
                candidates = provider.list().map_err(|error| error.to_string())?;
            }
            retain_pending_candidates(&mut candidates, record.total, |candidate| {
                provider.local_path_for(candidate)
            });
            if provider.kind() == "wallhaven" && candidates.len() < record.total {
                provider
                    .refresh(&transport, current_time_ms()?)
                    .map_err(|error| error.to_string())?;
                candidates = provider.list().map_err(|error| error.to_string())?;
                retain_pending_candidates(&mut candidates, record.total, |candidate| {
                    provider.local_path_for(candidate)
                });
            }
            if candidates.is_empty() {
                set_hydration_job_total(store, record, 0)?;
                return Ok(());
            }
            set_hydration_job_total(store, record, candidates.len())?;
            for candidate in &candidates {
                if advance_job_or_cancel(store, record)? {
                    return Ok(());
                }
                provider
                    .hydrate(candidate, &transport, current_time_ms()?)
                    .map_err(|error| error.to_string())?;
                increment_job(store, record)?;
            }
        }
    }
    Ok(())
}

fn retain_pending_candidates<T>(
    candidates: &mut Vec<T>,
    limit: usize,
    mut local_path_for: impl FnMut(&T) -> PathBuf,
) {
    candidates.retain(|candidate| !local_path_for(candidate).is_file());
    candidates.truncate(limit);
}

fn set_hydration_job_total(
    store: &JobStore,
    record: &JobRecord,
    total: usize,
) -> Result<(), String> {
    let mut latest = store.load(&record.id).map_err(|error| error.to_string())?;
    latest.total = total;
    latest.completed = 0;
    latest.updated_at_unix_ms = current_time_ms()?;
    store.save(&latest).map_err(|error| error.to_string())
}

fn advance_job_or_cancel(store: &JobStore, record: &JobRecord) -> Result<bool, String> {
    let mut latest = store.load(&record.id).map_err(|error| error.to_string())?;
    if latest.status != JobStatus::CancelRequested {
        return Ok(false);
    }
    latest.status = JobStatus::Canceled;
    latest.updated_at_unix_ms = current_time_ms()?;
    store.save(&latest).map_err(|error| error.to_string())?;
    Ok(true)
}

fn increment_job(store: &JobStore, record: &JobRecord) -> Result<(), String> {
    let mut latest = store.load(&record.id).map_err(|error| error.to_string())?;
    latest.completed = latest.completed.saturating_add(1).min(latest.total);
    latest.updated_at_unix_ms = current_time_ms()?;
    store.save(&latest).map_err(|error| error.to_string())
}

fn run_pack(action: &str, args: &[String]) -> Result<(), String> {
    let contract = contract_requested(args);
    match action {
        "list" if frontend_flags_only(args) => {
            let config = load_config().map_err(|error| error.to_string())?;
            let safe = sanitized_config(&config)?;
            emit_frontend_result(
                "pack list",
                serde_json::json!({
                    "packs": safe["packs"],
                    "providerCredentials": safe["providerCredentials"]
                }),
                contract,
            )
        }
        "show" => {
            let name = required_name(args.first(), "usage: kitowall pack show <name>")?;
            let config = load_config().map_err(|error| error.to_string())?;
            let pack = config
                .packs
                .get(&name)
                .ok_or_else(|| format!("pack not found: {name}"))?;
            emit_frontend_result(
                "pack show",
                serde_json::json!({
                    "name": name,
                    "pack": sanitized_pack(pack, config.provider_credentials(pack.kind()))?
                }),
                contract,
            )
        }
        "set-key" => {
            let name = required_name(
                args.first(),
                "usage: kitowall pack set-key <name> [--api-key <key>|--api-key-env <env>|--clear-api-key-env]",
            )?;
            let api_key = option_value(args, "--api-key").map(|value| non_empty(value, "--api-key")).transpose()?;
            let api_key_env = option_value(args, "--api-key-env")
                .map(validate_api_key_env)
                .transpose()?;
            let clear_api_key_env = args.iter().any(|value| value == "--clear-api-key-env");
            if api_key.is_none() && api_key_env.is_none() && !clear_api_key_env {
                return Err("provide --api-key, --api-key-env, or --clear-api-key-env".into());
            }
            let mut config = load_config().map_err(|error| error.to_string())?;
            let provider = config
                .packs
                .get(&name)
                .ok_or_else(|| format!("pack not found: {name}"))?
                .kind()
                .to_owned();
            if !matches!(provider.as_str(), "wallhaven" | "unsplash") {
                return Err(format!("pack type {provider} does not use an API key"));
            }
            config.promote_legacy_provider_credentials(&provider);
            update_provider_credentials(
                &mut config,
                &provider,
                api_key,
                api_key_env,
                clear_api_key_env,
            );
            config.clear_legacy_provider_credentials(&provider);
            save_config(&config).map_err(|error| error.to_string())?;
            let pack = &config.packs[&name];
            emit_frontend_result(
                "pack set-key",
                serde_json::json!({
                    "ok": true, "name": name,
                    "pack": sanitized_pack(pack, config.provider_credentials(&provider))?
                }),
                contract,
            )
        }
        "subtheme" => {
            let values = args
                .iter()
                .filter(|argument| !matches!(argument.as_str(), "--contract-v1" | "--json"))
                .collect::<Vec<_>>();
            if values.len() != 3 || !matches!(values[0].as_str(), "add" | "remove") {
                return Err("usage: kitowall pack subtheme <add|remove> <name> <value>".into());
            }
            let operation = values[0].as_str();
            let name = required_name(values.get(1).copied(), "invalid pack name")?;
            let value = non_empty(values[2], "subtheme")?;
            let mut config = load_config().map_err(|error| error.to_string())?;
            let pack = config
                .packs
                .get_mut(&name)
                .ok_or_else(|| format!("pack not found: {name}"))?;
            let subthemes = match pack {
                PackConfig::Wallhaven(pack) => pack.subthemes.get_or_insert_with(Vec::new),
                PackConfig::Reddit(pack) => pack.subthemes.get_or_insert_with(Vec::new),
                PackConfig::Unsplash(pack) => pack.subthemes.get_or_insert_with(Vec::new),
                _ => return Err(format!("pack type {} does not support subthemes", pack.kind())),
            };
            if operation == "add" {
                if !subthemes.contains(&value) { subthemes.push(value); }
            } else {
                subthemes.retain(|item| item != &value);
            }
            let result = subthemes.clone();
            save_config(&config).map_err(|error| error.to_string())?;
            emit_frontend_result(
                "pack subtheme",
                serde_json::json!({"ok": true, "name": name, "subthemes": result}),
                contract,
            )
        }
        "add" | "update" => upsert_pack(action, args, contract),
        "remove" => {
            let name = required_name(args.first(), "usage: kitowall pack remove <name>")?;
            let mut config = load_config().map_err(|error| error.to_string())?;
            let mut state = load_state().map_err(|error| error.to_string())?;
            let result = config
                .remove_pack(&mut state, &name)
                .map_err(|error| error.to_string())?;
            save_config(&config).map_err(|error| error.to_string())?;
            save_state(&state).map_err(|error| error.to_string())?;
            emit_frontend_result(
                "pack remove",
                if contract {
                    serde_json::json!({
                        "removed": result.removed,
                        "detached_from_pool": result.detached_from_pool,
                        "cleared_current_pack": result.cleared_current_pack
                    })
                } else {
                    serde_json::json!({
                        "ok": true,
                        "removed": result.removed,
                        "detachedFromPool": result.detached_from_pool,
                        "clearedCurrentPack": result.cleared_current_pack
                    })
                },
                contract,
            )
        }
        "status" => {
            let name = required_name(args.first(), "usage: kitowall pack status <name>")?;
            let config = load_config().map_err(|error| error.to_string())?;
            let pack = config
                .packs
                .get(&name)
                .ok_or_else(|| format!("pack not found: {name}"))?;
            match pack {
                PackConfig::Local { paths } => {
                    let candidates = LocalProvider::from_environment()
                        .map_err(|error| error.to_string())?
                        .discover(paths)
                        .map_err(|error| error.to_string())?;
                    emit_frontend_result(
                        "pack status",
                        serde_json::json!({
                            "ok": true,
                            "name": name,
                            "type": "local",
                            "count": candidates.len(),
                            "candidates": candidates
                        }),
                        contract,
                    )
                }
                PackConfig::StaticUrl(pack) => {
                    let provider = static_url_provider(&name, pack.clone(), &config)?;
                    let status = provider.status().map_err(|error| error.to_string())?;
                    let candidates = provider
                        .list_candidates()
                        .map_err(|error| error.to_string())?;
                    emit_frontend_result(
                        "pack status",
                        serde_json::json!({
                            "ok": true,
                            "name": name,
                            "type": "static_url",
                            "status": status,
                            "candidates": candidates
                        }),
                        contract,
                    )
                }
                _ => {
                    let provider = configured_provider(&name, pack, &config)?;
                    let status = provider.status().map_err(|error| error.to_string())?;
                    let candidates = provider.list().map_err(|error| error.to_string())?;
                    emit_frontend_result(
                        "pack status",
                        serde_json::json!({
                            "ok": true,
                            "name": name, "type": provider.kind(),
                            "status": status, "candidates": candidates
                        }),
                        contract,
                    )
                }
            }
        }
        "refresh" => {
            let name = required_name(args.first(), "usage: kitowall pack refresh <name>")?;
            let config = load_config().map_err(|error| error.to_string())?;
            let pack = config
                .packs
                .get(&name)
                .ok_or_else(|| format!("pack not found: {name}"))?;
            match pack {
                PackConfig::Local { .. } => emit_frontend_result(
                    "pack refresh",
                    serde_json::json!({
                        "ok": true, "name": name, "type": "local",
                        "refreshed": false, "reason": "local-pack"
                    }),
                    contract,
                ),
                PackConfig::StaticUrl(pack) => {
                    let provider = static_url_provider(&name, pack.clone(), &config)?;
                    let index = provider
                        .refresh_index(current_time_ms()?)
                        .map_err(|error| error.to_string())?;
                    let hydrated = if args.iter().any(|argument| argument == "--hydrate") {
                        hydrate_static_url(&provider, args)?
                    } else {
                        Vec::new()
                    };
                    emit_frontend_result(
                        "pack refresh",
                        serde_json::json!({
                            "ok": true, "name": name, "type": "static_url",
                            "refreshed": true, "count": index.candidates.len(),
                            "hydrated": hydrated.len(), "paths": hydrated
                        }),
                        contract,
                    )
                }
                _ => {
                    let provider = configured_provider(&name, pack, &config)?;
                    let transport = UreqTransport::default();
                    let count = provider
                        .refresh(&transport, current_time_ms()?)
                        .map_err(|error| error.to_string())?;
                    let hydrated = if args.iter().any(|argument| argument == "--hydrate") {
                        hydrate_configured_provider(&provider, args)?
                    } else {
                        Vec::new()
                    };
                    emit_frontend_result(
                        "pack refresh",
                        serde_json::json!({
                            "ok": true, "name": name, "type": provider.kind(),
                            "refreshed": true, "count": count,
                            "hydrated": hydrated.len(), "paths": hydrated
                        }),
                        contract,
                    )
                }
            }
        }
        "hydrate" => {
            let name = required_name(
                args.first(),
                "usage: kitowall pack hydrate <name> [--count <n>]",
            )?;
            let config = load_config().map_err(|error| error.to_string())?;
            let pack = config
                .packs
                .get(&name)
                .ok_or_else(|| format!("pack not found: {name}"))?;
            match pack {
                PackConfig::StaticUrl(pack) => {
                    let provider = static_url_provider(&name, pack.clone(), &config)?;
                    let paths = hydrate_static_url(&provider, args)?;
                    emit_frontend_result(
                        "pack hydrate",
                        serde_json::json!({
                            "ok": true, "name": name, "type": "static_url",
                            "hydrated": paths.len(), "paths": paths
                        }),
                        contract,
                    )
                }
                PackConfig::Local { .. } => Err("local packs do not require hydration".into()),
                _ => {
                    let provider = configured_provider(&name, pack, &config)?;
                    let paths = hydrate_configured_provider(&provider, args)?;
                    emit_frontend_result(
                        "pack hydrate",
                        serde_json::json!({
                            "ok": true, "name": name, "type": provider.kind(),
                            "hydrated": paths.len(), "paths": paths
                        }),
                        contract,
                    )
                }
            }
        }
        _ => Err(
            "usage: kitowall pack <list|show|add|update|remove|status|refresh|hydrate|set-key|subtheme> ...".into(),
        ),
    }
}

fn hydrate_static_url(
    provider: &StaticUrlProvider,
    args: &[String],
) -> Result<Vec<String>, String> {
    let count = option_value(args, "--count")
        .map(|value| parse_positive(value, "--count"))
        .transpose()?
        .unwrap_or(1) as usize;
    if count > 100 {
        return Err("--count cannot exceed 100 downloads per invocation".into());
    }
    let transport = UreqTransport::default();
    let mut candidates = provider
        .list_candidates()
        .map_err(|error| error.to_string())?;
    if candidates.is_empty() {
        provider
            .refresh_index(current_time_ms()?)
            .map_err(|error| error.to_string())?;
        candidates = provider
            .list_candidates()
            .map_err(|error| error.to_string())?;
    }
    if candidates.is_empty() {
        return Err("pack refresh completed without candidates".into());
    }
    let now_ms = current_time_ms()?;
    candidates
        .iter()
        .take(count)
        .map(|candidate| {
            provider
                .hydrate(candidate, &transport, now_ms)
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn hydrate_configured_provider(
    provider: &ConfiguredProvider,
    args: &[String],
) -> Result<Vec<String>, String> {
    let count = hydration_count(args)?;
    let transport = UreqTransport::default();
    let mut candidates = provider.list().map_err(|error| error.to_string())?;
    if candidates.is_empty() {
        provider
            .refresh(&transport, current_time_ms()?)
            .map_err(|error| error.to_string())?;
        candidates = provider.list().map_err(|error| error.to_string())?;
    }
    if candidates.is_empty() {
        return Err("pack refresh completed without candidates".into());
    }
    let now_ms = current_time_ms()?;
    candidates
        .iter()
        .take(count)
        .map(|candidate| {
            provider
                .hydrate(candidate, &transport, now_ms)
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn hydration_count(args: &[String]) -> Result<usize, String> {
    let count = option_value(args, "--count")
        .map(|value| parse_positive(value, "--count"))
        .transpose()?
        .unwrap_or(1) as usize;
    if count > 100 {
        Err("--count cannot exceed 100 downloads per invocation".into())
    } else {
        Ok(count)
    }
}

fn upsert_pack(action: &str, args: &[String], contract: bool) -> Result<(), String> {
    let name = required_name(
        args.first(),
        &format!("usage: kitowall pack {action} <name> --type <provider> [options]"),
    )?;
    let mut config = load_config().map_err(|error| error.to_string())?;
    let kind = option_value(args, "--type")
        .or_else(|| config.packs.get(&name).map(PackConfig::kind))
        .ok_or_else(|| "missing --type".to_owned())?;
    let kind = kind.to_owned();
    if matches!(kind.as_str(), "wallhaven" | "unsplash") {
        config.promote_legacy_provider_credentials(&kind);
        let api_key = option_value(args, "--api-key")
            .map(|value| non_empty(value, "--api-key"))
            .transpose()?;
        let api_key_env = option_value(args, "--api-key-env")
            .map(validate_api_key_env)
            .transpose()?;
        update_provider_credentials(
            &mut config,
            &kind,
            api_key,
            api_key_env,
            args.iter().any(|value| value == "--clear-api-key-env"),
        );
    }
    let existing = config.packs.get(&name);
    let mut pack = match kind.as_str() {
        "local" => {
            let paths = option_value(args, "--paths")
                .map(parse_list)
                .or_else(|| match existing {
                    Some(PackConfig::Local { paths }) => Some(paths.clone()),
                    _ => None,
                })
                .filter(|paths| !paths.is_empty())
                .ok_or_else(|| "local pack requires --paths <comma-separated-list>".to_owned())?;
            PackConfig::Local { paths }
        }
        "static_url" => PackConfig::StaticUrl(build_static_url_pack(args, existing)?),
        "wallhaven" => PackConfig::Wallhaven(build_wallhaven_pack(args, existing)?),
        "reddit" => PackConfig::Reddit(build_reddit_pack(args, existing)?),
        "unsplash" => PackConfig::Unsplash(build_unsplash_pack(args, existing)?),
        "generic_json" => PackConfig::GenericJson(build_generic_json_pack(args, existing)?),
        _ => return Err(format!("unsupported pack type: {kind}")),
    };
    if matches!(kind.as_str(), "wallhaven" | "unsplash")
        && config
            .provider_credentials(&kind)
            .is_some_and(|credentials| credentials.is_configured())
    {
        pack.clear_credentials();
        config.clear_legacy_provider_credentials(&kind);
    }
    let normalized = if action == "add" {
        config.add_pack(&name, pack)
    } else {
        config.update_pack(&name, pack)
    }
    .map_err(|error| error.to_string())?;
    save_config(&config).map_err(|error| error.to_string())?;
    let saved_pack = &config.packs[&normalized];
    let safe_pack = sanitized_pack(saved_pack, config.provider_credentials(saved_pack.kind()))?;
    emit_frontend_result(
        &format!("pack {action}"),
        serde_json::json!({
            "ok": true,
            "action": action,
            "name": normalized,
            "pack": safe_pack
        }),
        contract,
    )
}

fn build_static_url_pack(
    args: &[String],
    existing: Option<&PackConfig>,
) -> Result<StaticUrlPack, String> {
    let mut pack = match existing {
        Some(PackConfig::StaticUrl(pack)) => pack.clone(),
        _ => StaticUrlPack::default(),
    };
    if let Some(url) = option_value(args, "--url") {
        pack.url = Some(non_empty(url, "--url")?);
    }
    if let Some(urls) = option_value(args, "--urls") {
        pack.urls = Some(parse_list(urls));
    }
    if let Some(value) = option_value(args, "--different-images") {
        pack.different_images = Some(parse_bool(value, "--different-images")?);
    }
    if let Some(value) = option_value(args, "--count") {
        pack.count = Some(parse_positive(value, "--count")?);
    }
    if let Some(value) = option_value(args, "--ttl-sec") {
        pack.ttl_sec = Some(parse_positive(value, "--ttl-sec")? as u64);
    }
    for (option, field) in [
        ("--author-name", &mut pack.author_name),
        ("--author-url", &mut pack.author_url),
        ("--domain", &mut pack.domain),
        ("--post-url", &mut pack.post_url),
    ] {
        if let Some(value) = option_value(args, option) {
            *field = Some(non_empty(value, option)?);
        }
    }
    Ok(pack)
}

fn build_wallhaven_pack(
    args: &[String],
    existing: Option<&PackConfig>,
) -> Result<WallhavenPack, String> {
    let mut pack = match existing {
        Some(PackConfig::Wallhaven(pack)) => pack.clone(),
        _ => WallhavenPack::default(),
    };
    set_string(args, "--api-key", &mut pack.api_key)?;
    set_api_key_env(args, &mut pack.api_key_env)?;
    set_string(args, "--keyword", &mut pack.keyword)?;
    set_list(args, "--subthemes", &mut pack.subthemes);
    set_string(args, "--categories", &mut pack.categories)?;
    set_string(args, "--purity", &mut pack.purity)?;
    set_bool(args, "--allow-sfw", &mut pack.allow_sfw)?;
    set_bool(args, "--allow-sketchy", &mut pack.allow_sketchy)?;
    set_bool(args, "--allow-nsfw", &mut pack.allow_nsfw)?;
    set_bool(args, "--category-general", &mut pack.category_general)?;
    set_bool(args, "--category-anime", &mut pack.category_anime)?;
    set_bool(args, "--category-people", &mut pack.category_people)?;
    set_list(args, "--ratios", &mut pack.ratios);
    set_string(args, "--colors", &mut pack.colors)?;
    set_string(args, "--atleast", &mut pack.atleast)?;
    set_string(args, "--sorting", &mut pack.sorting)?;
    set_bool(args, "--ai-art", &mut pack.ai_art)?;
    set_u64(args, "--ttl-sec", &mut pack.ttl_sec)?;
    Ok(pack)
}

fn build_reddit_pack(args: &[String], existing: Option<&PackConfig>) -> Result<RedditPack, String> {
    let mut pack = match existing {
        Some(PackConfig::Reddit(pack)) => pack.clone(),
        _ => RedditPack::default(),
    };
    if let Some(value) = option_value(args, "--subreddits") {
        pack.subreddits = Some(StringOrList::List(parse_list(value)));
    }
    set_list(args, "--subthemes", &mut pack.subthemes);
    set_bool(args, "--allow-sfw", &mut pack.allow_sfw)?;
    set_u32(args, "--min-width", &mut pack.min_width)?;
    set_u32(args, "--min-height", &mut pack.min_height)?;
    set_f64(args, "--ratio-w", &mut pack.ratio_w)?;
    set_f64(args, "--ratio-h", &mut pack.ratio_h)?;
    set_string(args, "--sort", &mut pack.sort)?;
    set_string(args, "--time", &mut pack.time)?;
    set_u64(args, "--ttl-sec", &mut pack.ttl_sec)?;
    Ok(pack)
}

fn build_unsplash_pack(
    args: &[String],
    existing: Option<&PackConfig>,
) -> Result<UnsplashPack, String> {
    let mut pack = match existing {
        Some(PackConfig::Unsplash(pack)) => pack.clone(),
        _ => UnsplashPack::default(),
    };
    set_string(args, "--api-key", &mut pack.api_key)?;
    set_api_key_env(args, &mut pack.api_key_env)?;
    set_string(args, "--query", &mut pack.query)?;
    set_list(args, "--subthemes", &mut pack.subthemes);
    set_string(args, "--topics", &mut pack.topics)?;
    set_string(args, "--collections", &mut pack.collections)?;
    set_string(args, "--username", &mut pack.username)?;
    set_string(args, "--orientation", &mut pack.orientation)?;
    set_string(args, "--content-filter", &mut pack.content_filter)?;
    set_u32(args, "--image-width", &mut pack.image_width)?;
    set_u32(args, "--image-height", &mut pack.image_height)?;
    set_string(args, "--image-fit", &mut pack.image_fit)?;
    set_u32(args, "--image-quality", &mut pack.image_quality)?;
    set_u64(args, "--ttl-sec", &mut pack.ttl_sec)?;
    Ok(pack)
}

fn build_generic_json_pack(
    args: &[String],
    existing: Option<&PackConfig>,
) -> Result<GenericJsonPack, String> {
    let mut pack = match existing {
        Some(PackConfig::GenericJson(pack)) => pack.clone(),
        _ => GenericJsonPack::default(),
    };
    set_string(args, "--endpoint", &mut pack.endpoint)?;
    set_string(args, "--image-path", &mut pack.image_path)?;
    set_string(args, "--image-prefix", &mut pack.image_prefix)?;
    if let Some(value) = option_value(args, "--candidate-limit") {
        pack.candidate_limit = Some(parse_positive(value, "--candidate-limit")? as usize);
    }
    set_string(args, "--post-path", &mut pack.post_path)?;
    set_string(args, "--post-prefix", &mut pack.post_prefix)?;
    set_string(args, "--author-name-path", &mut pack.author_name_path)?;
    set_string(args, "--author-url-path", &mut pack.author_url_path)?;
    set_string(args, "--author-url-prefix", &mut pack.author_url_prefix)?;
    set_string(args, "--domain", &mut pack.domain)?;
    set_u64(args, "--ttl-sec", &mut pack.ttl_sec)?;
    Ok(pack)
}

fn set_string(args: &[String], option: &str, target: &mut Option<String>) -> Result<(), String> {
    if let Some(value) = option_value(args, option) {
        *target = Some(non_empty(value, option)?);
    }
    Ok(())
}

fn set_api_key_env(args: &[String], target: &mut Option<String>) -> Result<(), String> {
    if args.iter().any(|value| value == "--clear-api-key-env") {
        *target = None;
    }
    if let Some(value) = option_value(args, "--api-key-env") {
        *target = Some(validate_api_key_env(value)?);
    }
    Ok(())
}

fn validate_api_key_env(value: &str) -> Result<String, String> {
    let value = non_empty(value, "--api-key-env")?;
    let mut chars = value.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest = chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if valid_start && valid_rest {
        Ok(value)
    } else {
        Err("--api-key-env must be an environment variable name such as WALLHAVEN_KEY".into())
    }
}

fn set_list(args: &[String], option: &str, target: &mut Option<Vec<String>>) {
    if let Some(value) = option_value(args, option) {
        *target = Some(parse_list(value));
    }
}

fn set_bool(args: &[String], option: &str, target: &mut Option<bool>) -> Result<(), String> {
    if let Some(value) = option_value(args, option) {
        *target = Some(parse_bool(value, option)?);
    }
    Ok(())
}

fn set_u32(args: &[String], option: &str, target: &mut Option<u32>) -> Result<(), String> {
    if let Some(value) = option_value(args, option) {
        *target = Some(parse_positive(value, option)?);
    }
    Ok(())
}

fn set_u64(args: &[String], option: &str, target: &mut Option<u64>) -> Result<(), String> {
    if let Some(value) = option_value(args, option) {
        *target = Some(parse_positive(value, option)? as u64);
    }
    Ok(())
}

fn set_f64(args: &[String], option: &str, target: &mut Option<f64>) -> Result<(), String> {
    if let Some(value) = option_value(args, option) {
        *target = Some(
            value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite() && *number > 0.0)
                .ok_or_else(|| format!("invalid {option}: {value}"))?,
        );
    }
    Ok(())
}

fn static_url_provider(
    name: &str,
    pack: StaticUrlPack,
    config: &kitowall_backend::Config,
) -> Result<StaticUrlProvider, String> {
    let cache = CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
    Ok(StaticUrlProvider::new(name, pack, cache))
}

fn configured_provider(
    name: &str,
    pack: &PackConfig,
    config: &kitowall_backend::Config,
) -> Result<ConfiguredProvider, String> {
    let cache = CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
    ConfiguredProvider::from_pack(name, pack, config.provider_credentials(pack.kind()), cache)
        .ok_or_else(|| format!("remote provider unavailable for type: {}", pack.kind()))
}

fn update_provider_credentials(
    config: &mut kitowall_backend::Config,
    provider: &str,
    api_key: Option<String>,
    api_key_env: Option<String>,
    clear_api_key_env: bool,
) {
    let credentials = config
        .provider_credentials
        .entry(provider.to_owned())
        .or_default();
    if let Some(api_key) = api_key {
        credentials.api_key = Some(api_key);
    }
    if let Some(api_key_env) = api_key_env {
        credentials.api_key_env = Some(api_key_env);
    } else if clear_api_key_env {
        credentials.api_key_env = None;
    }
    if !credentials.is_configured() {
        config.provider_credentials.remove(provider);
    }
}

fn list_packs() -> Result<(), String> {
    let config = load_config().map_err(|error| error.to_string())?;
    let provider = LocalProvider::from_environment().map_err(|error| error.to_string())?;
    let rows = config
        .packs
        .iter()
        .map(|(name, pack)| {
            let count = match pack {
                PackConfig::Local { paths } => {
                    provider.discover(paths).ok().map(|items| items.len())
                }
                PackConfig::StaticUrl(pack) => static_url_provider(name, pack.clone(), &config)
                    .ok()
                    .and_then(|provider| provider.status().ok())
                    .map(|status| status.candidates),
                _ => configured_provider(name, pack, &config)
                    .ok()
                    .and_then(|provider| provider.status().ok())
                    .map(|status| status.candidates),
            };
            serde_json::json!({"name": name, "type": pack.kind(), "count": count})
        })
        .collect::<Vec<_>>();
    print_json(&serde_json::json!({
        "packs": rows,
        "pool": {"enabled": config.pool.enabled, "sources": config.pool.sources}
    }))
}

fn required_name(value: Option<&String>, usage: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| usage.to_owned())?;
    let normalized = normalize_pack_name(value);
    if normalized.is_empty() {
        Err(usage.to_owned())
    } else {
        Ok(normalized)
    }
}

fn option_value<'a>(args: &'a [String], option: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == option)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_wallpaper_map(value: &str) -> Result<Vec<ApplyWallpaperTarget>, String> {
    let targets = value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (output, id) = entry
                .split_once(':')
                .ok_or_else(|| format!("invalid wallpaper map entry: {entry}"))?;
            Ok(ApplyWallpaperTarget {
                output: non_empty(output, "output")?,
                id: non_empty(id, "wallpaper id")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if targets.is_empty() {
        Err("wallpaper map cannot be empty".into())
    } else {
        Ok(targets)
    }
}

fn contract_requested(args: &[String]) -> bool {
    args.iter().any(|argument| argument == "--contract-v1")
}

fn frontend_flags_only(args: &[String]) -> bool {
    args.iter()
        .all(|argument| matches!(argument.as_str(), "--contract-v1" | "--json"))
}

fn frontend_flags_only_except(args: &[String], valued: &[&str]) -> bool {
    let mut index = 0;
    while index < args.len() {
        if matches!(args[index].as_str(), "--contract-v1" | "--json") {
            index += 1;
            continue;
        }
        if valued.contains(&args[index].as_str()) && index + 1 < args.len() {
            index += 2;
            continue;
        }
        return false;
    }
    true
}

fn capability_commands() -> Vec<&'static str> {
    vec![
        "version",
        "capabilities",
        "config show",
        "config init",
        "doctor",
        "status",
        "outputs",
        "wallpaper list",
        "wallpaper apply",
        "wallpaper apply-batch",
        "dashboard snapshot",
        "next",
        "rotate-now",
        "mode",
        "transition set",
        "settings get",
        "settings set",
        "favorite list",
        "favorite add",
        "favorite remove",
        "history list",
        "history clear",
        "logs list",
        "logs clear",
        "job list",
        "job status",
        "job start",
        "job cancel",
        "watch",
        "service plan",
        "service apply",
        "pack list",
        "pack show",
        "pack add",
        "pack update",
        "pack remove",
        "pack status",
        "pack refresh",
        "pack hydrate",
        "pack set-key",
        "pack subtheme",
        "list-packs",
        "cache status",
        "cache plan",
        "cache prune",
    ]
}

fn capabilities_legacy_data() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "product": "kitowall",
        "commands": capability_commands(),
        "packTypes": ["local", "wallhaven", "reddit", "unsplash", "generic_json", "static_url"],
        "writablePackTypes": ["local", "static_url", "generic_json", "reddit", "wallhaven", "unsplash"]
    })
}

fn capabilities_contract_data() -> serde_json::Value {
    serde_json::json!({
        "product": "kitowall",
        "features": {
            "static_wallpapers": true,
            "multi_output": true,
            "remote_providers": true,
            "previews": true,
            "favorites": true,
            "history": true,
            "rotation": true,
            "service_automation": true,
            "live_wallpapers": false,
            "audio_spectrum": false
        },
        "commands": capability_commands(),
        "pack_types": ["local", "wallhaven", "reddit", "unsplash", "generic_json", "static_url"],
        "writable_pack_types": ["local", "static_url", "generic_json", "reddit", "wallhaven", "unsplash"]
    })
}

fn non_empty(value: &str, option: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{option} cannot be empty"))
    } else {
        Ok(value.into())
    }
}

fn parse_bool(value: &str, option: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("invalid {option}: {value}")),
    }
}

fn parse_positive(value: &str, option: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| format!("invalid {option}: {value}"))
}

fn parse_nonnegative(value: &str, option: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid {option}: {value}"))
}

fn required_cli_option<'a>(args: &'a [String], option: &str) -> Result<&'a str, String> {
    option_value(args, option).ok_or_else(|| format!("missing required option: {option}"))
}

fn parse_range_f64(value: &str, option: &str, min: f64, max: f64) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number >= min && *number <= max)
        .ok_or_else(|| format!("invalid {option}: {value} (expected {min}..={max})"))
}

fn current_time_ms() -> Result<u64, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis() as u64)
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn emit_frontend_result<T: Serialize>(
    command: &str,
    data: T,
    contract: bool,
) -> Result<(), String> {
    if contract {
        print_json(&ContractSuccess {
            schema_version: 1,
            ok: true,
            command: command.into(),
            data,
            warnings: Vec::new(),
            meta: contract_meta(),
        })
    } else {
        print_json(&data)
    }
}

fn contract_meta() -> ContractMeta {
    ContractMeta {
        cli: "kitowall",
        cli_version: env!("CARGO_PKG_VERSION"),
        contract_version: CONTRACT_VERSION,
    }
}

fn command_name(args: &[String]) -> String {
    match args {
        [group, action, ..] if !action.starts_with("--") => format!("{group} {action}"),
        [command, ..] => command.clone(),
        [] => "help".into(),
    }
}

fn classify_error(error: &str) -> (&'static str, u8, Option<&'static str>) {
    let lower = error.to_ascii_lowercase();
    if lower.contains("missing required option")
        || lower.starts_with("usage:")
        || lower.contains("invalid --")
        || lower.contains("limit must be")
    {
        ("INVALID_ARGUMENT", 2, None)
    } else if lower.contains("output not found") {
        (
            "OUTPUT_NOT_FOUND",
            3,
            Some("Run `kitowall outputs` to list available outputs"),
        )
    } else if lower.contains("pack not found") || lower.contains("wallpaper not found") {
        ("RESOURCE_NOT_FOUND", 3, None)
    } else if lower.contains("failed to execute") || lower.contains("executable not found") {
        ("DEPENDENCY_MISSING", 4, None)
    } else if lower.contains("compositor") || lower.contains("no outputs detected") {
        ("COMPOSITOR_UNAVAILABLE", 5, None)
    } else if lower.contains("http")
        || lower.contains("provider")
        || lower.contains("transport")
        || lower.contains("renderer")
    {
        ("PROVIDER_FAILED", 8, None)
    } else if lower.contains("io")
        || lower.contains("permission")
        || lower.contains("config")
        || lower.contains("state")
    {
        ("IO_ERROR", 7, None)
    } else {
        ("INTERNAL_ERROR", 10, None)
    }
}

fn sanitized_config(config: &kitowall_backend::Config) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(config).map_err(|error| error.to_string())?;
    if let Some(credentials) = value
        .get_mut("providerCredentials")
        .and_then(serde_json::Value::as_object_mut)
    {
        for credential in credentials.values_mut() {
            sanitize_credential_value(credential);
        }
    }
    if let Some(packs) = value
        .get_mut("packs")
        .and_then(serde_json::Value::as_object_mut)
    {
        for pack in packs.values_mut() {
            let provider = pack
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            sanitize_pack_value(pack, config.provider_credentials(provider));
        }
    }
    Ok(value)
}

fn sanitized_pack(
    pack: &PackConfig,
    credentials: Option<&kitowall_backend::ProviderCredentials>,
) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(pack).map_err(|error| error.to_string())?;
    sanitize_pack_value(&mut value, credentials);
    Ok(value)
}

fn sanitize_pack_value(
    value: &mut serde_json::Value,
    credentials: Option<&kitowall_backend::ProviderCredentials>,
) {
    let Some(pack) = value.as_object_mut() else {
        return;
    };
    let has_inline_key = pack
        .remove("apiKey")
        .and_then(|value| value.as_str().map(|value| !value.trim().is_empty()))
        .unwrap_or(false);
    let valid_env_key = pack
        .get("apiKeyEnv")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| validate_api_key_env(value).ok());
    if valid_env_key.is_none() {
        pack.remove("apiKeyEnv");
    }
    let shared_env = credentials
        .and_then(|credentials| credentials.api_key_env.as_deref())
        .and_then(|value| validate_api_key_env(value).ok());
    if valid_env_key.is_none() {
        if let Some(variable) = &shared_env {
            pack.insert(
                "apiKeyEnv".into(),
                serde_json::Value::String(variable.clone()),
            );
        }
    }
    pack.insert(
        "apiKeyConfigured".into(),
        serde_json::Value::Bool(
            has_inline_key
                || valid_env_key.is_some()
                || credentials.is_some_and(|credentials| credentials.is_configured()),
        ),
    );
}

fn sanitize_credential_value(value: &mut serde_json::Value) {
    let Some(credentials) = value.as_object_mut() else {
        return;
    };
    let has_inline_key = credentials
        .remove("apiKey")
        .and_then(|value| value.as_str().map(|value| !value.trim().is_empty()))
        .unwrap_or(false);
    let valid_env_key = credentials
        .get("apiKeyEnv")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| validate_api_key_env(value).ok());
    if valid_env_key.is_none() {
        credentials.remove("apiKeyEnv");
    }
    credentials.insert(
        "apiKeyConfigured".into(),
        serde_json::Value::Bool(has_inline_key || valid_env_key.is_some()),
    );
}

fn usage() -> &'static str {
    "usage: kitowall <command> [options] [--lc]\n\
--lc resolves kitsune-compositor from the local refactor workspace"
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitowall_backend::config::{ProviderCredentials, WallhavenPack};

    #[test]
    fn parses_comma_separated_paths() {
        assert_eq!(
            parse_list(" ~/Pictures, /srv/walls, "),
            ["~/Pictures", "/srv/walls"]
        );
    }

    #[test]
    fn finds_the_refactor_root_from_the_local_target_tree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let nested = root.join("kitowall/target/debug");
        assert_eq!(find_refactor_root(&nested), Some(root.to_path_buf()));
    }

    #[test]
    fn option_parser_requires_a_following_value() {
        assert_eq!(option_value(&["--paths".into()], "--paths"), None);
    }

    #[test]
    fn hydration_selects_only_missing_candidates_up_to_the_limit() {
        let root =
            std::env::temp_dir().join(format!("kitowall-pending-{}", current_time_ms().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("downloaded"), b"wallpaper").unwrap();
        let mut candidates = vec![
            "downloaded".to_owned(),
            "pending-one".to_owned(),
            "pending-two".to_owned(),
            "pending-three".to_owned(),
        ];

        retain_pending_candidates(&mut candidates, 2, |candidate| root.join(candidate));

        assert_eq!(candidates, ["pending-one", "pending-two"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_output_never_exposes_inline_api_keys() {
        let mut config = kitowall_backend::Config::default();
        config.packs.insert(
            "wallhaven".into(),
            PackConfig::Wallhaven(WallhavenPack {
                api_key: Some("secret-token".into()),
                api_key_env: Some("WALLHAVEN_KEY".into()),
                keyword: Some("landscape".into()),
                ..WallhavenPack::default()
            }),
        );
        let safe = sanitized_config(&config).unwrap();
        let pack = &safe["packs"]["wallhaven"];
        assert!(pack.get("apiKey").is_none());
        assert_eq!(pack["apiKeyConfigured"], true);
        assert_eq!(pack["apiKeyEnv"], "WALLHAVEN_KEY");
    }

    #[test]
    fn shared_provider_key_is_redacted_and_visible_as_configured_to_packs() {
        let mut config = kitowall_backend::Config::default();
        config.provider_credentials.insert(
            "wallhaven".into(),
            ProviderCredentials {
                api_key: Some("shared-secret".into()),
                api_key_env: None,
            },
        );
        config.packs.insert(
            "landscape".into(),
            PackConfig::Wallhaven(WallhavenPack {
                keyword: Some("landscape".into()),
                ..WallhavenPack::default()
            }),
        );

        let safe = sanitized_config(&config).unwrap();
        assert!(safe["providerCredentials"]["wallhaven"]
            .get("apiKey")
            .is_none());
        assert_eq!(
            safe["providerCredentials"]["wallhaven"]["apiKeyConfigured"],
            true
        );
        assert_eq!(safe["packs"]["landscape"]["apiKeyConfigured"], true);
        assert!(!safe.to_string().contains("shared-secret"));
    }

    #[test]
    fn invalid_environment_key_names_are_rejected_and_sanitized() {
        assert!(validate_api_key_env("WALLHAVEN_KEY").is_ok());
        assert!(validate_api_key_env("token-value").is_err());
        let pack = PackConfig::Wallhaven(WallhavenPack {
            api_key_env: Some("token-value".into()),
            ..WallhavenPack::default()
        });
        let safe = sanitized_pack(&pack, None).unwrap();
        assert!(safe.get("apiKeyEnv").is_none());
        assert_eq!(safe["apiKeyConfigured"], false);
    }

    #[test]
    fn compositor_apply_arguments_include_the_complete_transition() {
        let args = wallpaper_apply_args(&WallpaperApply {
            namespace: "kitowall".into(),
            output: "DP-1".into(),
            image: "/tmp/wall.png".into(),
            transition: kitowall_backend::config::TransitionConfig {
                kind: "wipe".into(),
                fps: 120,
                duration: 0.0,
                angle: Some(45.5),
                pos: Some("0.5,0.5".into()),
            },
        });
        assert_eq!(args[0..2], ["wallpaper", "apply"]);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--transition-type", "wipe"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--transition-duration", "0"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--transition-angle", "45.5"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--transition-pos", "0.5,0.5"]));
        assert_eq!(args.last().unwrap(), "--contract-v1");
    }

    #[test]
    fn static_service_model_has_four_portable_tasks() {
        let intents = automation_intents(
            "/opt/kitowall/bin/kitowall",
            "/opt/kitsune/bin/kitsune-compositor",
            "kitowall",
            600,
        );
        assert_eq!(intents.len(), 4);
        assert_eq!(intents[0].id, "kitowall-runtime");
        assert_eq!(intents[1].id, "kitowall-next");
        assert_eq!(intents[2].id, "kitowall-watch");
        assert_eq!(intents[3].id, "kitowall-login-apply");
        assert_eq!(intents[1].schedule.as_ref().unwrap().every_seconds, 600);
        assert_eq!(intents[2].kind, "daemon");
        assert!(intents[2].autostart);
        let request = AutomationBatchIntent {
            schema_version: 1,
            automations: intents,
        };
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains("\"schema_version\":1"));
        assert!(text.contains("\"automations\""));
        assert!(!text.contains(".service"));
        assert!(!text.contains(".timer"));
        assert!(!text.contains("WantedBy"));
    }

    #[test]
    fn service_status_normalizes_active_and_missing_automations() {
        let active = normalize_automation_status(
            "kitowall-watch",
            "Cambios de monitores",
            "Watch outputs",
            true,
            &serde_json::json!({
                "data": {
                    "artifacts": [{
                        "id": "kitowall-watch",
                        "unit_name": "kitowall-watch.service",
                        "installed": true,
                        "enabled": true,
                        "active": true
                    }]
                }
            }),
        );
        assert_eq!(active.state, "active");
        assert!(active.installed);
        assert!(active.enabled);
        assert!(active.active);

        let missing = normalize_automation_status(
            "kitowall-runtime",
            "Runtime estatico",
            "Static runtime",
            false,
            &serde_json::json!({
                "error": {
                    "message": "automation id not found: kitowall-runtime"
                }
            }),
        );
        assert_eq!(missing.state, "not_installed");
        assert!(!missing.installed);
        assert_eq!(missing.artifacts.len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn automation_batch_request_is_written_as_a_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let request = AutomationBatchIntent {
            schema_version: 1,
            automations: automation_intents("/bin/true", "/bin/true", "kitowall", 600),
        };
        let path = write_automation_batch_request(&request).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["automations"].as_array().unwrap().len(), 4);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn frontend_contract_classifies_domain_errors() {
        assert_eq!(
            classify_error("output not found: DP-9"),
            (
                "OUTPUT_NOT_FOUND",
                3,
                Some("Run `kitowall outputs` to list available outputs")
            )
        );
        assert_eq!(
            classify_error("pack not found: missing").0,
            "RESOURCE_NOT_FOUND"
        );
        assert_eq!(
            command_name(&["wallpaper".into(), "list".into(), "--contract-v1".into()]),
            "wallpaper list"
        );
    }
}
