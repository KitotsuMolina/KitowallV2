use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::config::{Mode, TransitionConfig};
use crate::{pick_images_for_outputs, Config, HttpTransport, ResolvedPool, State};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallpaperApply {
    pub namespace: String,
    pub output: String,
    pub image: String,
    pub transition: TransitionConfig,
}

pub trait WallpaperGateway {
    fn outputs(&self) -> Result<Vec<String>, String>;
    fn apply(&self, request: &WallpaperApply) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyNextOptions {
    pub force: bool,
    pub namespace: String,
    pub now_ms: u64,
    pub start_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyNextResult {
    pub pack: String,
    pub outputs: Vec<String>,
    pub images: Vec<crate::OutputPick>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyWallpaperOptions {
    pub id: String,
    pub output: String,
    pub namespace: String,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyWallpaperResult {
    pub id: String,
    pub pack: String,
    pub output: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyWallpaperBatchOptions {
    pub targets: Vec<ApplyWallpaperTarget>,
    pub namespace: String,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyWallpaperTarget {
    pub output: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyWallpaperBatchResult {
    pub pack: String,
    pub images: Vec<ApplyWallpaperResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApplyOutcome {
    Applied(ApplyNextResult),
    Skipped { reason: String },
}

pub fn apply_next<T: HttpTransport, G: WallpaperGateway>(
    config: &Config,
    state: &mut State,
    pool: &ResolvedPool,
    transport: &T,
    gateway: &G,
    options: &ApplyNextOptions,
) -> Result<ApplyOutcome, String> {
    if state.mode == Mode::Manual && !options.force {
        return Ok(ApplyOutcome::Skipped {
            reason: "mode=manual".into(),
        });
    }
    let outputs = gateway.outputs()?;
    if outputs.is_empty() {
        return Err("no outputs detected".into());
    }
    if pool.paths.is_empty() {
        return Err(format!("no images found for pack: {}", pool.name));
    }
    let mut next_state = state.clone();
    next_state.cleanup_disconnected_outputs(&outputs);
    let picks = pick_images_for_outputs(
        &outputs,
        &pool.paths,
        config,
        &next_state,
        options.start_offset,
    );
    if picks.len() != outputs.len() {
        return Err(format!(
            "could not select an image for every output in pack: {}",
            pool.name
        ));
    }

    for pick in &picks {
        pool.hydrate(&pick.path, transport, options.now_ms)?;
    }
    for pick in &picks {
        gateway.apply(&WallpaperApply {
            namespace: options.namespace.clone(),
            output: pick.output.clone(),
            image: pick.path.clone(),
            transition: config.transition.clone(),
        })?;
    }
    for pick in &picks {
        next_state.commit_selection(&pick.output, &pick.path, options.now_ms);
    }
    next_state.current_pack = Some(pool.name.clone());
    *state = next_state;
    Ok(ApplyOutcome::Applied(ApplyNextResult {
        pack: pool.name.clone(),
        outputs,
        images: picks,
    }))
}

pub fn apply_wallpaper<T: HttpTransport, G: WallpaperGateway>(
    config: &Config,
    state: &mut State,
    pool: &ResolvedPool,
    transport: &T,
    gateway: &G,
    options: &ApplyWallpaperOptions,
) -> Result<ApplyWallpaperResult, String> {
    let outputs = gateway.outputs()?;
    if !outputs.iter().any(|output| output == &options.output) {
        return Err(format!("output not found: {}", options.output));
    }
    let path = pool
        .path_for_id(&options.id)
        .ok_or_else(|| format!("wallpaper not found in pack {}: {}", pool.name, options.id))?
        .to_owned();
    let hydrated = pool
        .hydrate(&path, transport, options.now_ms)?
        .to_string_lossy()
        .into_owned();
    gateway.apply(&WallpaperApply {
        namespace: options.namespace.clone(),
        output: options.output.clone(),
        image: hydrated.clone(),
        transition: config.transition.clone(),
    })?;

    let mut next_state = state.clone();
    next_state.cleanup_disconnected_outputs(&outputs);
    next_state.commit_selection(&options.output, &hydrated, options.now_ms);
    next_state.current_pack = Some(pool.name.clone());
    *state = next_state;
    Ok(ApplyWallpaperResult {
        id: options.id.clone(),
        pack: pool.name.clone(),
        output: options.output.clone(),
        path: hydrated,
    })
}

pub fn apply_wallpaper_batch<T: HttpTransport, G: WallpaperGateway>(
    config: &Config,
    state: &mut State,
    pool: &ResolvedPool,
    transport: &T,
    gateway: &G,
    options: &ApplyWallpaperBatchOptions,
) -> Result<ApplyWallpaperBatchResult, String> {
    if options.targets.is_empty() {
        return Err("wallpaper batch cannot be empty".into());
    }
    let mut requested_outputs = BTreeSet::new();
    for target in &options.targets {
        if !requested_outputs.insert(&target.output) {
            return Err(format!(
                "duplicate output in wallpaper batch: {}",
                target.output
            ));
        }
    }
    let outputs = gateway.outputs()?;
    for output in &requested_outputs {
        if !outputs.contains(output) {
            return Err(format!("output not found: {output}"));
        }
    }
    let mut resolved = Vec::new();
    for target in &options.targets {
        let path = pool
            .path_for_id(&target.id)
            .ok_or_else(|| format!("wallpaper not found in pack {}: {}", pool.name, target.id))?;
        let hydrated = pool
            .hydrate(path, transport, options.now_ms)?
            .to_string_lossy()
            .into_owned();
        resolved.push((target, hydrated));
    }

    for (target, path) in &resolved {
        gateway.apply(&WallpaperApply {
            namespace: options.namespace.clone(),
            output: target.output.clone(),
            image: path.clone(),
            transition: config.transition.clone(),
        })?;
    }
    let mut next_state = state.clone();
    next_state.cleanup_disconnected_outputs(&outputs);
    let images = resolved
        .into_iter()
        .map(|(target, path)| {
            next_state.commit_selection(&target.output, &path, options.now_ms);
            ApplyWallpaperResult {
                id: target.id.clone(),
                pack: pool.name.clone(),
                output: target.output.clone(),
                path,
            }
        })
        .collect();
    next_state.current_pack = Some(pool.name.clone());
    *state = next_state;
    Ok(ApplyWallpaperBatchResult {
        pack: pool.name.clone(),
        images,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PackConfig;
    use crate::HttpResponse;
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct NoHttp;

    impl HttpTransport for NoHttp {
        fn get(&self, _url: &str) -> Result<HttpResponse, crate::TransportError> {
            panic!("local controller test must not use HTTP")
        }
    }

    struct FakeGateway {
        outputs: Vec<String>,
        requests: RefCell<Vec<WallpaperApply>>,
        fail: bool,
    }

    impl WallpaperGateway for FakeGateway {
        fn outputs(&self) -> Result<Vec<String>, String> {
            Ok(self.outputs.clone())
        }

        fn apply(&self, request: &WallpaperApply) -> Result<(), String> {
            self.requests.borrow_mut().push(request.clone());
            if self.fail {
                Err("renderer failed".into())
            } else {
                Ok(())
            }
        }
    }

    fn fixture() -> (PathBuf, Config, ResolvedPool) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kitowall-controller-{id}"));
        fs::create_dir_all(&root).unwrap();
        for name in ["a.png", "b.png"] {
            fs::write(root.join(name), b"image").unwrap();
        }
        let mut config = Config {
            mode: Mode::Rotate,
            ..Config::default()
        };
        config.packs.insert(
            "local".into(),
            PackConfig::Local {
                paths: vec![root.to_string_lossy().into_owned()],
            },
        );
        let pool = ResolvedPool::resolve(&config, Some("local"), None, &root, &NoHttp, 1).unwrap();
        (root, config, pool)
    }

    #[test]
    fn applies_every_output_and_commits_after_success() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into(), "HDMI-A-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut state = State {
            mode: Mode::Rotate,
            ..State::default()
        };
        let outcome = apply_next(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyNextOptions {
                force: false,
                namespace: "kitowall".into(),
                now_ms: 10,
                start_offset: 0,
            },
        )
        .unwrap();
        assert!(matches!(outcome, ApplyOutcome::Applied(_)));
        assert_eq!(gateway.requests.borrow().len(), 2);
        assert_eq!(state.current_pack.as_deref(), Some("local"));
        assert_eq!(state.last_set.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn renderer_failure_does_not_commit_state() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: true,
        };
        let mut state = State {
            mode: Mode::Rotate,
            ..State::default()
        };
        let before = state.clone();
        assert!(apply_next(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyNextOptions {
                force: false,
                namespace: "kitowall".into(),
                now_ms: 10,
                start_offset: 0,
            },
        )
        .is_err());
        assert_eq!(state, before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn manual_mode_skips_without_contacting_the_gateway() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut state = State::default();
        let outcome = apply_next(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyNextOptions {
                force: false,
                namespace: "kitowall".into(),
                now_ms: 10,
                start_offset: 0,
            },
        )
        .unwrap();
        assert!(matches!(outcome, ApplyOutcome::Skipped { .. }));
        assert!(gateway.requests.borrow().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn applies_a_catalog_item_directly_and_commits_after_success() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: false,
        };
        let path = pool.paths[0].clone();
        let id = crate::catalog::wallpaper_id(&pool.name, &path);
        let mut state = State::default();
        let result = apply_wallpaper(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyWallpaperOptions {
                id: id.clone(),
                output: "DP-1".into(),
                namespace: "kitowall".into(),
                now_ms: 20,
            },
        )
        .unwrap();
        assert_eq!(result.id, id);
        assert_eq!(result.path, path);
        assert_eq!(state.last_set["DP-1"], path);
        assert_eq!(gateway.requests.borrow().len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_apply_rejects_unknown_outputs_without_mutating_state() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut state = State::default();
        let before = state.clone();
        let result = apply_wallpaper(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyWallpaperOptions {
                id: crate::catalog::wallpaper_id(&pool.name, &pool.paths[0]),
                output: "DP-9".into(),
                namespace: "kitowall".into(),
                now_ms: 20,
            },
        );
        assert!(result.unwrap_err().contains("output not found"));
        assert_eq!(state, before);
        assert!(gateway.requests.borrow().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn applies_a_direct_batch_and_commits_only_after_all_requests() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into(), "HDMI-A-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: false,
        };
        let mut state = State::default();
        let result = apply_wallpaper_batch(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyWallpaperBatchOptions {
                targets: vec![
                    ApplyWallpaperTarget {
                        output: "DP-1".into(),
                        id: crate::catalog::wallpaper_id(&pool.name, &pool.paths[0]),
                    },
                    ApplyWallpaperTarget {
                        output: "HDMI-A-1".into(),
                        id: crate::catalog::wallpaper_id(&pool.name, &pool.paths[1]),
                    },
                ],
                namespace: "kitowall".into(),
                now_ms: 30,
            },
        )
        .unwrap();
        assert_eq!(result.images.len(), 2);
        assert_eq!(state.last_set.len(), 2);
        assert_eq!(gateway.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_direct_batch_does_not_commit_state() {
        let (root, config, pool) = fixture();
        let gateway = FakeGateway {
            outputs: vec!["DP-1".into()],
            requests: RefCell::new(Vec::new()),
            fail: true,
        };
        let mut state = State::default();
        let before = state.clone();
        let result = apply_wallpaper_batch(
            &config,
            &mut state,
            &pool,
            &NoHttp,
            &gateway,
            &ApplyWallpaperBatchOptions {
                targets: vec![ApplyWallpaperTarget {
                    output: "DP-1".into(),
                    id: crate::catalog::wallpaper_id(&pool.name, &pool.paths[0]),
                }],
                namespace: "kitowall".into(),
                now_ms: 30,
            },
        );
        assert!(result.is_err());
        assert_eq!(state, before);
        let _ = fs::remove_dir_all(root);
    }
}
