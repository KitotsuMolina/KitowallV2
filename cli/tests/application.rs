use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "kitowall-cli-application-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[cfg(unix)]
fn fake_compositor(root: &Path, log: &Path) -> PathBuf {
    let path = root.join("kitsune-compositor-fake");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = outputs ]; then\n\
               printf '%s\\n' '{{\"schema_version\":1,\"ok\":true,\"data\":{{\"outputs\":[{{\"name\":\"DP-1\"}}]}}}}'\n\
               exit 0\n\
             fi\n\
             printf '%s\\n' \"$*\" >> '{}'\n\
             printf '%s\\n' '{{\"schema_version\":1,\"ok\":true,\"data\":{{}}}}'\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn run(root: &Path, compositor: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kitowall"))
        .args(args)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("KITSUNE_COMPOSITOR_BIN", compositor)
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn local_flag_is_global_and_preserves_the_contract() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let compositor = fake_compositor(&root, &root.join("compositor.log"));
    let output = run(&root, &compositor, &["--lc", "version", "--contract-v1"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["meta"]["cli"], "kitowall");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn dashboard_snapshot_tracks_local_catalog_changes_and_revision() {
    let root = temp_root();
    let walls = root.join("walls");
    fs::create_dir_all(&walls).unwrap();
    fs::write(walls.join("first.png"), b"image").unwrap();
    let compositor = fake_compositor(&root, &root.join("compositor.log"));
    let added = run(
        &root,
        &compositor,
        &[
            "pack",
            "add",
            "local",
            "--type",
            "local",
            "--paths",
            walls.to_string_lossy().as_ref(),
            "--contract-v1",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let first = run(
        &root,
        &compositor,
        &["dashboard", "snapshot", "--contract-v1"],
    );
    assert!(first.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["data"]["counts"]["available"], 1);
    assert_eq!(first["data"]["counts"]["downloaded"], 1);
    assert_eq!(first["data"]["counts"]["local"], 1);
    assert!(first["data"]["watchPaths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some(walls.to_string_lossy().as_ref())));

    fs::write(walls.join("second.jpg"), b"image").unwrap();
    let second = run(
        &root,
        &compositor,
        &["dashboard", "snapshot", "--contract-v1"],
    );
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["data"]["counts"]["available"], 2);
    assert_ne!(first["data"]["revision"], second["data"]["revision"]);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn new_wallhaven_pack_reuses_and_migrates_a_legacy_provider_key() {
    let root = temp_root();
    let config_dir = root.join("config/kitowall");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "mode": "manual",
            "wallpaper_backend": "auto",
            "rotation_interval_seconds": 1800,
            "transition": {"type": "center", "fps": 60, "duration": 0.7},
            "selection": {"perOutputCooldown": 10, "globalCooldown": 20, "avoidSameTickDuplicates": true},
            "cache": {
                "dir": root.join("cache/kitowall"),
                "downloadDir": root.join("downloads"),
                "maxMB": 2048,
                "defaultTtlSec": 604800
            },
            "pool": {"enabled": false, "sources": [], "dedupe": "path"},
            "packs": {
                "legacy": {
                    "type": "wallhaven",
                    "apiKey": "legacy-secret",
                    "keyword": "anime"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let compositor = fake_compositor(&root, &root.join("compositor.log"));

    let added = run(
        &root,
        &compositor,
        &[
            "pack",
            "add",
            "landscape",
            "--type",
            "wallhaven",
            "--keyword",
            "landscape",
            "--contract-v1",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let config: Value =
        serde_json::from_slice(&fs::read(config_dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        config["providerCredentials"]["wallhaven"]["apiKey"],
        "legacy-secret"
    );
    assert!(config["packs"]["legacy"].get("apiKey").is_none());
    assert!(config["packs"]["landscape"].get("apiKey").is_none());

    let listed_output = run(&root, &compositor, &["pack", "list", "--contract-v1"]);
    assert!(listed_output.status.success());
    assert!(!String::from_utf8_lossy(&listed_output.stdout).contains("legacy-secret"));
    let listed: Value = serde_json::from_slice(&listed_output.stdout).unwrap();
    assert_eq!(
        listed["data"]["providerCredentials"]["wallhaven"]["apiKeyConfigured"],
        true
    );
    assert_eq!(
        listed["data"]["packs"]["landscape"]["apiKeyConfigured"],
        true
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn local_next_uses_the_compositor_contract_and_persists_only_success() {
    let root = temp_root();
    let walls = root.join("walls");
    let config_dir = root.join("config/kitowall");
    fs::create_dir_all(&walls).unwrap();
    fs::create_dir_all(&config_dir).unwrap();
    let image = walls.join("wall.png");
    fs::write(&image, b"image").unwrap();
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "mode": "manual",
            "wallpaper_backend": "auto",
            "rotation_interval_seconds": 1800,
            "transition": {"type": "center", "fps": 60, "duration": 0.7},
            "selection": {"perOutputCooldown": 10, "globalCooldown": 20, "avoidSameTickDuplicates": true},
            "cache": {
                "dir": root.join("cache/kitowall"),
                "downloadDir": root.join("downloads"),
                "maxMB": 2048,
                "defaultTtlSec": 604800
            },
            "pool": {"enabled": false, "sources": [], "dedupe": "path"},
            "packs": {"local": {"type": "local", "paths": [walls]}}
        }))
        .unwrap(),
    )
    .unwrap();
    let log = root.join("compositor.log");
    let compositor = fake_compositor(&root, &log);

    for args in [
        vec!["version", "--contract-v1"],
        vec!["capabilities", "--contract-v1"],
        vec!["status", "--contract-v1"],
        vec!["config", "show", "--contract-v1"],
        vec!["outputs", "--contract-v1"],
        vec!["doctor", "--contract-v1"],
        vec!["settings", "get", "--contract-v1"],
        vec!["favorite", "list", "--contract-v1"],
        vec!["history", "list", "--contract-v1"],
        vec!["cache", "status", "--contract-v1"],
        vec!["cache", "plan", "--contract-v1"],
        vec!["pack", "list", "--contract-v1"],
        vec!["pack", "show", "local", "--contract-v1"],
        vec!["pack", "status", "local", "--contract-v1"],
    ] {
        let output = run(&root, &compositor, &args);
        assert!(
            output.status.success(),
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], true);
        assert_eq!(value["meta"]["cli"], "kitowall");
        assert_eq!(value["meta"]["contract_version"], "1.0");
    }

    let cached = root.join("downloads/remote/expired.jpg");
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"expired").unwrap();
    fs::create_dir_all(root.join("cache/kitowall")).unwrap();
    fs::write(
        root.join("cache/kitowall/index.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "entries": [{
                "key": "remote:expired",
                "localPath": cached,
                "sizeBytes": 7,
                "addedAt": 0,
                "ttlSec": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let prune = run(
        &root,
        &compositor,
        &["cache", "prune", "--confirm", "--contract-v1"],
    );
    assert!(
        prune.status.success(),
        "{}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let prune: Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(prune["command"], "cache prune");
    assert_eq!(prune["data"]["removed_entries"], 1);
    assert_eq!(prune["data"]["removed_files"], 1);
    assert!(!cached.exists());

    let catalog = run(
        &root,
        &compositor,
        &[
            "wallpaper",
            "list",
            "--pack",
            "local",
            "--limit",
            "10",
            "--contract-v1",
        ],
    );
    assert!(
        catalog.status.success(),
        "{}",
        String::from_utf8_lossy(&catalog.stderr)
    );
    let catalog: Value = serde_json::from_slice(&catalog.stdout).unwrap();
    assert_eq!(catalog["schema_version"], 1);
    assert_eq!(catalog["command"], "wallpaper list");
    assert_eq!(catalog["data"]["total"], 1);
    let wallpaper_id = catalog["data"]["items"][0]["id"].as_str().unwrap();
    let direct = run(
        &root,
        &compositor,
        &[
            "wallpaper",
            "apply",
            "--pack",
            "local",
            "--id",
            wallpaper_id,
            "--output",
            "DP-1",
            "--contract-v1",
        ],
    );
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let direct: Value = serde_json::from_slice(&direct.stdout).unwrap();
    assert_eq!(direct["command"], "wallpaper apply");
    assert_eq!(direct["data"]["output"], "DP-1");
    assert_eq!(direct["data"]["path"], image.to_string_lossy().as_ref());
    let batch = run(
        &root,
        &compositor,
        &[
            "wallpaper",
            "apply-batch",
            "--pack",
            "local",
            "--map",
            &format!("DP-1:{wallpaper_id}"),
            "--contract-v1",
        ],
    );
    assert!(
        batch.status.success(),
        "{}",
        String::from_utf8_lossy(&batch.stderr)
    );
    let batch: Value = serde_json::from_slice(&batch.stdout).unwrap();
    assert_eq!(batch["command"], "wallpaper apply-batch");
    assert_eq!(batch["data"]["images"][0]["output"], "DP-1");
    let invalid_output = run(
        &root,
        &compositor,
        &[
            "wallpaper",
            "apply",
            "--pack",
            "local",
            "--id",
            wallpaper_id,
            "--output",
            "DP-9",
            "--contract-v1",
        ],
    );
    assert_eq!(invalid_output.status.code(), Some(3));
    let invalid_output: Value = serde_json::from_slice(&invalid_output.stdout).unwrap();
    assert_eq!(invalid_output["ok"], false);
    assert_eq!(invalid_output["error"]["code"], "OUTPUT_NOT_FOUND");
    let clear = run(&root, &compositor, &["history", "clear"]);
    assert!(clear.status.success());

    let mode = run(&root, &compositor, &["mode", "manual", "--contract-v1"]);
    assert!(mode.status.success());
    let mode: Value = serde_json::from_slice(&mode.stdout).unwrap();
    assert_eq!(mode["command"], "mode");
    let transition = run(
        &root,
        &compositor,
        &[
            "transition",
            "set",
            "--type",
            "wipe",
            "--duration",
            "0.5",
            "--contract-v1",
        ],
    );
    assert!(transition.status.success());
    let transition: Value = serde_json::from_slice(&transition.stdout).unwrap();
    assert_eq!(transition["command"], "transition set");
    let settings = run(
        &root,
        &compositor,
        &[
            "settings",
            "set",
            "--rotation-interval-seconds",
            "900",
            "--contract-v1",
        ],
    );
    assert!(settings.status.success());
    let settings: Value = serde_json::from_slice(&settings.stdout).unwrap();
    assert_eq!(settings["data"]["rotation_interval_seconds"], 900);

    let added = run(
        &root,
        &compositor,
        &[
            "pack",
            "add",
            "ui-local",
            "--type",
            "local",
            "--paths",
            walls.to_str().unwrap(),
            "--contract-v1",
        ],
    );
    assert!(added.status.success());
    let added: Value = serde_json::from_slice(&added.stdout).unwrap();
    assert_eq!(added["command"], "pack add");
    let refreshed = run(
        &root,
        &compositor,
        &["pack", "refresh", "ui-local", "--contract-v1"],
    );
    assert!(refreshed.status.success());
    let removed = run(
        &root,
        &compositor,
        &["pack", "remove", "ui-local", "--contract-v1"],
    );
    assert!(removed.status.success());

    let started = run(
        &root,
        &compositor,
        &["job", "start", "refresh", "local", "--contract-v1"],
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let started: Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(started["command"], "job start");
    let job_id = started["data"]["id"].as_str().unwrap();
    let mut terminal = None;
    for _ in 0..50 {
        let status = run(
            &root,
            &compositor,
            &["job", "status", job_id, "--contract-v1"],
        );
        assert!(status.status.success());
        let status: Value = serde_json::from_slice(&status.stdout).unwrap();
        if matches!(
            status["data"]["status"].as_str(),
            Some("completed" | "failed" | "canceled")
        ) {
            terminal = Some(status);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let terminal = terminal.expect("refresh job did not reach a terminal state");
    assert_eq!(terminal["data"]["status"], "completed");
    assert_eq!(terminal["data"]["completed"], 1);
    let jobs = run(&root, &compositor, &["job", "list", "--contract-v1"]);
    assert!(jobs.status.success());
    let jobs: Value = serde_json::from_slice(&jobs.stdout).unwrap();
    assert_eq!(jobs["data"]["jobs"][0]["id"], job_id);

    assert!(run(&root, &compositor, &["mode", "rotate"])
        .status
        .success());
    assert!(run(
        &root,
        &compositor,
        &[
            "transition",
            "set",
            "--enabled",
            "false",
            "--type",
            "wipe",
            "--angle",
            "45.5",
            "--pos",
            "0.5,0.5",
        ],
    )
    .status
    .success());
    let output = run(
        &root,
        &compositor,
        &["next", "--pack", "local", "--contract-v1"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["command"], "next");
    assert_eq!(response["data"]["outcome"], "applied");
    assert_eq!(response["data"]["pack"], "local");
    assert_eq!(response["data"]["images"][0]["output"], "DP-1");

    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains("wallpaper apply --namespace kitowall --output DP-1"));
    assert!(calls.contains("--transition-type wipe"));
    assert!(calls.contains("--transition-duration 0"));
    assert!(calls.contains("--transition-angle 45.5"));
    assert!(calls.contains("--transition-pos 0.5,0.5"));
    assert!(calls.contains("--contract-v1"));

    let state: Value =
        serde_json::from_slice(&fs::read(root.join("state/kitowall/state.json")).unwrap()).unwrap();
    assert_eq!(state["current_pack"], "local");
    assert_eq!(state["last_set"]["DP-1"], image.to_string_lossy().as_ref());

    let history = run(&root, &compositor, &["history", "list", "--limit", "1"]);
    assert!(history.status.success());
    let history: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(
        history["entries"][0]["path"],
        image.to_string_lossy().as_ref()
    );
    assert_eq!(history["entries"][0]["favorite"], false);

    assert!(run(
        &root,
        &compositor,
        &["favorite", "add", image.to_str().unwrap()],
    )
    .status
    .success());
    let history = run(&root, &compositor, &["history", "--limit", "1"]);
    let history: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history["entries"][0]["favorite"], true);
    let favorites = run(&root, &compositor, &["favorites"]);
    let favorites: Value = serde_json::from_slice(&favorites.stdout).unwrap();
    assert_eq!(favorites["favorites"][0], image.to_string_lossy().as_ref());

    let clear = run(&root, &compositor, &["history", "clear"]);
    let clear: Value = serde_json::from_slice(&clear.stdout).unwrap();
    assert_eq!(clear["removed"], 1);

    let logs = run(
        &root,
        &compositor,
        &["logs", "list", "--source", "rotation", "--contract-v1"],
    );
    assert!(logs.status.success());
    let logs: Value = serde_json::from_slice(&logs.stdout).unwrap();
    assert_eq!(logs["command"], "logs list");
    assert!(!logs["data"]["entries"].as_array().unwrap().is_empty());
    let clear_logs = run(&root, &compositor, &["logs", "clear", "--contract-v1"]);
    assert!(clear_logs.status.success());
    let clear_logs: Value = serde_json::from_slice(&clear_logs.stdout).unwrap();
    assert_eq!(clear_logs["command"], "logs clear");

    let services = run(
        &root,
        &compositor,
        &["service", "plan", "--every-seconds", "600", "--contract-v1"],
    );
    assert!(
        services.status.success(),
        "{}",
        String::from_utf8_lossy(&services.stderr)
    );
    let services: Value = serde_json::from_slice(&services.stdout).unwrap();
    assert_eq!(services["command"], "service plan");
    assert_eq!(services["data"]["action"], "plan");
    assert!(services["data"]["batch"].is_object());
    assert_eq!(services["data"]["activation_required"], false);
    let calls = fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("automation plan-batch --descriptor").count(),
        1
    );
    let status = run(&root, &compositor, &["service", "status", "--contract-v1"]);
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["data"]["automations"].as_array().unwrap().len(), 4);
    let calls = fs::read_to_string(&log).unwrap();
    assert_eq!(calls.matches("automation status --id").count(), 8);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn doctor_is_read_only_when_configuration_is_missing() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let log = root.join("compositor.log");
    let compositor = fake_compositor(&root, &log);
    let output = run(&root, &compositor, &["doctor", "--contract-v1"]);
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "doctor");
    assert_eq!(value["data"]["checks"][0]["id"], "config");
    assert_eq!(value["data"]["checks"][0]["severity"], "warning");
    assert!(!root.join("config/kitowall/config.json").exists());
    assert!(!root.join("state/kitowall/state.json").exists());
    let _ = fs::remove_dir_all(root);
}
