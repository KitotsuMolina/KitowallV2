use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::Mode;

pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const RECENT_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub mode: Mode,
    pub current_pack: Option<String>,
    #[serde(default)]
    pub last_outputs: Vec<String>,
    #[serde(default)]
    pub last_set: BTreeMap<String, String>,
    pub last_updated: u64,
    #[serde(default)]
    pub recent_by_output: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub recent_global: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            mode: Mode::Manual,
            current_pack: None,
            last_outputs: Vec::new(),
            last_set: BTreeMap::new(),
            last_updated: 0,
            recent_by_output: BTreeMap::new(),
            recent_global: Vec::new(),
        }
    }
}

impl State {
    pub fn set_mode(&mut self, mode: Mode, now_ms: u64) {
        self.mode = mode;
        self.last_updated = now_ms;
    }

    pub fn cleanup_disconnected_outputs(&mut self, current_outputs: &[String]) {
        let alive: BTreeSet<_> = current_outputs.iter().collect();
        self.last_set.retain(|output, _| alive.contains(output));
        self.recent_by_output
            .retain(|output, _| alive.contains(output));
        self.last_outputs = current_outputs.to_vec();
        self.trim_recent();
    }

    pub fn commit_selection(&mut self, output: &str, path: &str, now_ms: u64) {
        self.last_set.insert(output.into(), path.into());
        push_recent(
            self.recent_by_output.entry(output.into()).or_default(),
            path,
        );
        push_recent(&mut self.recent_global, path);
        self.last_updated = now_ms;
        self.trim_recent();
    }

    pub fn trim_recent(&mut self) {
        trim_to_limit(&mut self.recent_global);
        for queue in self.recent_by_output.values_mut() {
            trim_to_limit(queue);
        }
    }
}

fn push_recent(queue: &mut Vec<String>, value: &str) {
    queue.retain(|item| item != value);
    queue.push(value.into());
    trim_to_limit(queue);
}

fn trim_to_limit(queue: &mut Vec<String>) {
    if queue.len() > RECENT_LIMIT {
        queue.drain(..queue.len() - RECENT_LIMIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_are_deduplicated_and_capped() {
        let mut state = State::default();
        for index in 0..15 {
            state.commit_selection("DP-1", &format!("/{index}.png"), index);
        }
        state.commit_selection("DP-1", "/14.png", 16);
        assert_eq!(state.recent_global.len(), RECENT_LIMIT);
        assert_eq!(state.recent_global.last().unwrap(), "/14.png");
        assert_eq!(state.recent_by_output["DP-1"].len(), RECENT_LIMIT);
    }

    #[test]
    fn disconnected_outputs_are_removed() {
        let mut state = State::default();
        state.commit_selection("DP-1", "/a.png", 1);
        state.commit_selection("HDMI-A-1", "/b.png", 2);
        state.cleanup_disconnected_outputs(&["DP-1".into()]);
        assert!(!state.last_set.contains_key("HDMI-A-1"));
        assert!(!state.recent_by_output.contains_key("HDMI-A-1"));
    }
}
