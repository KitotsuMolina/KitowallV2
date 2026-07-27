use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{config::SelectionConfig, Config, State};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPick {
    pub output: String,
    pub path: String,
}

pub fn pick_images_for_outputs(
    outputs: &[String],
    pool: &[String],
    config: &Config,
    state: &State,
    start_offset: usize,
) -> Vec<OutputPick> {
    if pool.is_empty() {
        return Vec::new();
    }

    let selection = &config.selection;
    let banned_global = tail_set(&state.recent_global, selection.global_cooldown);
    let mut used = BTreeSet::new();
    let mut result = Vec::new();

    for output in outputs {
        let recent = state
            .recent_by_output
            .get(output)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let banned_output = tail_set(recent, selection.per_output_cooldown);
        let chosen = choose(
            pool,
            start_offset,
            selection,
            &banned_output,
            &banned_global,
            &used,
        );
        if let Some(path) = chosen {
            used.insert(path.clone());
            result.push(OutputPick {
                output: output.clone(),
                path,
            });
        }
    }
    result
}

fn choose(
    pool: &[String],
    start_offset: usize,
    selection: &SelectionConfig,
    banned_output: &BTreeSet<String>,
    banned_global: &BTreeSet<String>,
    used: &BTreeSet<String>,
) -> Option<String> {
    for stage in 0..5 {
        for index in 0..pool.len() {
            let candidate = &pool[(start_offset + index) % pool.len()];
            let unique = !selection.avoid_same_tick_duplicates || !used.contains(candidate);
            let allowed = match stage {
                0 => {
                    !banned_output.contains(candidate)
                        && !banned_global.contains(candidate)
                        && unique
                }
                1 => !banned_output.contains(candidate) && unique,
                2 => !banned_global.contains(candidate) && unique,
                3 => unique,
                _ => true,
            };
            if allowed {
                return Some(candidate.clone());
            }
        }
    }
    None
}

fn tail_set(values: &[String], count: usize) -> BTreeSet<String> {
    values[values.len().saturating_sub(count)..]
        .iter()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_cooldowns_and_tick_uniqueness() {
        let config = Config::default();
        let mut state = State::default();
        state.commit_selection("DP-1", "/a.png", 1);
        let picks = pick_images_for_outputs(
            &["DP-1".into(), "HDMI-A-1".into()],
            &["/a.png".into(), "/b.png".into(), "/c.png".into()],
            &config,
            &state,
            0,
        );
        assert_eq!(picks[0].path, "/b.png");
        assert_eq!(picks[1].path, "/c.png");
    }

    #[test]
    fn degrades_safely_when_pool_is_too_small() {
        let picks = pick_images_for_outputs(
            &["DP-1".into(), "HDMI-A-1".into()],
            &["/only.png".into()],
            &Config::default(),
            &State::default(),
            0,
        );
        assert_eq!(picks.len(), 2);
        assert_eq!(picks[0].path, picks[1].path);
    }
}
