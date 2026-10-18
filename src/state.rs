//! Run state shared between the updater and the TUI.
//!
//! The TUI is a READ-ONLY viewer: the guard writes this file as it works and the
//! `watch` command renders it. Nothing in the TUI can influence an update, so a
//! viewer crash can never strand an install.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const STAGES: [&str; 5] = ["preflight", "export", "backup", "update", "verify"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageState {
    Waiting,
    Active,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub name: String,
    pub state: StageState,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub state: StageState,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub version: String,
    pub run_id: String,
    pub started_at: String,
    pub updated_at: String,
    pub stages: Vec<Stage>,
    pub profiles: Vec<Profile>,
    pub blockers: Vec<String>,
    pub log_path: String,
    pub guard_pid: u32,
    pub child_pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub finished_at: Option<String>,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// `<home>/backups/guard-run.json` — one file, always the latest run.
pub fn state_path(home: &Path) -> PathBuf {
    home.join("backups").join("guard-run.json")
}

impl RunState {
    pub fn new(home: &Path, version: &str) -> Self {
        let run_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let log_path = home.join("backups").join("guard-update.log");
        Self {
            version: version.to_string(),
            run_id,
            started_at: now(),
            updated_at: now(),
            stages: STAGES
                .iter()
                .map(|s| Stage {
                    name: s.to_string(),
                    state: StageState::Waiting,
                    started_at: None,
                    finished_at: None,
                })
                .collect(),
            profiles: Vec::new(),
            blockers: Vec::new(),
            log_path: log_path.to_string_lossy().to_string(),
            guard_pid: std::process::id(),
            child_pid: None,
            exit_code: None,
            finished_at: None,
        }
    }

    pub fn seed_profiles(&mut self, names: &[String]) {
        self.profiles = names
            .iter()
            .map(|n| Profile {
                name: n.clone(),
                state: StageState::Waiting,
                detail: String::new(),
            })
            .collect();
    }

    pub fn stage(&mut self, name: &str, state: StageState) {
        if let Some(s) = self.stages.iter_mut().find(|s| s.name == name) {
            s.state = state;
            match state {
                StageState::Active => s.started_at = Some(now()),
                StageState::Done | StageState::Failed => s.finished_at = Some(now()),
                StageState::Waiting => {}
            }
        }
        self.updated_at = now();
    }

    pub fn profile(&mut self, name: &str, state: StageState, detail: &str) {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.name == name) {
            p.state = state;
            p.detail = detail.to_string();
        } else {
            self.profiles.push(Profile {
                name: name.to_string(),
                state,
                detail: detail.to_string(),
            });
        }
        self.updated_at = now();
    }

    pub fn blocker(&mut self, msg: &str) {
        if !self.blockers.iter().any(|b| b == msg) {
            self.blockers.push(msg.to_string());
        }
        self.updated_at = now();
    }

    pub fn finish(&mut self, code: i32) {
        self.exit_code = Some(code);
        self.finished_at = Some(now());
        self.updated_at = now();
    }

    pub fn write(&self, home: &Path) -> Result<()> {
        let p = state_path(home);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // write-then-rename so the viewer never reads a half-written file
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    pub fn read(home: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(state_path(home))?;
        let text = text.trim_start_matches('\u{feff}');
        Ok(serde_json::from_str(text)?)
    }
}

/// Tail the last `n` lines of the log the guard writes, tolerating absence.
pub fn tail_log(path: &str, n: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// Seconds since a `%Y-%m-%dT%H:%M:%S` stamp, or 0 when it cannot be parsed.
pub fn elapsed_since(stamp: &str) -> i64 {
    match chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S") {
        Ok(t) => {
            let then = t.and_local_timezone(chrono::Local).single();
            then.map(|d| (chrono::Local::now() - d).num_seconds())
                .unwrap_or(0)
        }
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_tracks_stage_times() {
        let root = std::env::temp_dir().join(format!("hug-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut s = RunState::new(&root, "0.3.0");
        s.seed_profiles(&["alpha".to_string(), "beta".to_string()]);
        s.stage("preflight", StageState::Active);
        s.stage("preflight", StageState::Done);
        s.profile("alpha", StageState::Done, "state.db 12.4 MB");
        s.blocker("venv held by pid 123");
        s.blocker("venv held by pid 123"); // deduped
        s.finish(0);
        s.write(&root).unwrap();

        let back = RunState::read(&root).unwrap();
        assert_eq!(back.profiles.len(), 2);
        assert_eq!(back.profiles[0].state, StageState::Done);
        assert_eq!(back.blockers.len(), 1, "blockers are deduped");
        assert_eq!(back.exit_code, Some(0));
        let pf = back.stages.iter().find(|s| s.name == "preflight").unwrap();
        assert_eq!(pf.state, StageState::Done);
        assert!(pf.started_at.is_some() && pf.finished_at.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tail_log_handles_missing_file() {
        assert!(tail_log("C:/definitely-not-here.log", 10).is_empty());
    }
}
