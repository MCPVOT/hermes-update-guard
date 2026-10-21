// Pop-up dashboard for a running guarded update.
//
// WHY A SEPARATE WINDOW
//   `hermes update` closes the Hermes desktop app (the preflight gate insists on
//   it), and the ratatui `watch` view needs a terminal. This window is neither:
//   it is its own WebView2 process, it survives the desktop closing, and it can
//   sit on top of whatever the operator is doing.
//
// READ-ONLY BY CONSTRUCTION
//   The only command is `snapshot()`: it reads `<home>/backups/guard-run.json` and
//   the tail of the guard's log. It cannot modify a run, so this window can never
//   strand an install — the same rule the TUI follows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

struct Home(PathBuf);

/// Resolve the Hermes home the same way the guard binary does.
fn default_home() -> PathBuf {
    if let Ok(h) = std::env::var("HERMES_HOME") {
        let p = PathBuf::from(&h);
        if p.join("profiles").is_dir() || p.join("hermes-agent").is_dir() {
            return p;
        }
    }
    if cfg!(windows) {
        if let Ok(la) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(la).join("hermes");
        }
        return PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".hermes");
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".hermes")
}

fn home_from_args() -> PathBuf {
    let mut args = std::env::args().skip(1);
    let mut explicit: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--home" => {
                if let Some(v) = args.next() {
                    explicit = Some(PathBuf::from(v));
                }
            }
            _ if a.starts_with("--home=") => {
                explicit = Some(PathBuf::from(a.trim_start_matches("--home=")));
            }
            _ => {}
        }
    }
    explicit.unwrap_or_else(default_home)
}

/// Last `max_lines` lines of a file, reading at most `max_bytes` from the end.
///
/// Bounded on purpose: a guard log can grow to megabytes during a node build, and
/// reading it whole on every 600 ms poll would make the window the problem.
fn tail(path: &Path, max_bytes: u64, max_lines: usize) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if start > 0 && !lines.is_empty() {
        // the first line is very likely cut mid-way
        lines.remove(0);
    }
    let n = lines.len();
    lines.split_off(n.saturating_sub(max_lines))
}

fn read_state_json(home: &Path) -> Result<Value, String> {
    let p = home.join("backups").join("guard-run.json");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str::<Value>(text).map_err(|e| format!("bad json in {}: {e}", p.display()))
}

#[tauri::command]
fn snapshot(state: tauri::State<Home>) -> Value {
    let home = &state.0;
    match read_state_json(home) {
        Ok(st) => {
            let log_path = st
                .get("log_path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("backups").join("guard-update.log"));
            json!({
                "ok": true,
                "home": home.display().to_string(),
                "state": st,
                "log": tail(&log_path, 512 * 1024, 400),
                "now": chrono::Local::now().format("%H:%M:%S").to_string(),
                "error": Value::Null,
            })
        }
        Err(e) => json!({
            "ok": false,
            "home": home.display().to_string(),
            "state": Value::Null,
            "log": Vec::<String>::new(),
            "now": chrono::Local::now().format("%H:%M:%S").to_string(),
            "error": e,
        }),
    }
}

#[tauri::command]
fn set_always_on_top(window: tauri::Window, on: bool) -> Result<(), String> {
    window.set_always_on_top(on).map_err(|e| e.to_string())
}

fn main() {
    let home = home_from_args();
    tauri::Builder::default()
        .manage(Home(home))
        .invoke_handler(tauri::generate_handler![snapshot, set_always_on_top])
        .run(tauri::generate_context!())
        .expect("failed to start the dashboard window");
}
