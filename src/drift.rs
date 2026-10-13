use anyhow::{bail, Context, Result};
use chrono::Local;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::env::{self, PROFILES};

/// Surface keys — verbatim from hermes-provider-drift.ps1 (SURFACE_KEYS).
/// Also protects the language keys the operator flagged (updates must never flip English away).
pub const SURFACE_KEYS: [&str; 12] = [
    "model.provider",
    "model.default",
    "model.base_url",
    "model.api_key",
    "auxiliary",
    "fallback_providers",
    "fallback_model",
    "delegation",
    "providers",
    "moa",
    "display.language",
    "stt.language",
];

/// Extract the provider surface from a config.yaml into a masked JSON object.
/// Port of the PS1 EXTRACT python: get dotted paths, mask api keys, truncate deep values.
pub fn extract_surface(cfg_path: &Path) -> Result<Value> {
    let txt = std::fs::read_to_string(cfg_path)
        .with_context(|| format!("read {}", cfg_path.display()))?;
    let yaml: Value =
        serde_yaml::from_str(&txt).with_context(|| format!("parse yaml {}", cfg_path.display()))?;
    let mut out = BTreeMap::new();
    for key in SURFACE_KEYS {
        let v = get_path(&yaml, key);
        out.insert(key.to_string(), short(mask(v)));
    }
    Ok(serde_json::to_value(out)?)
}

fn get_path(v: &Value, dotted: &str) -> Value {
    let mut cur = v;
    for seg in dotted.split('.') {
        match cur.get(seg) {
            Some(next) => cur = next,
            None => return Value::Null,
        }
    }
    cur.clone()
}

fn mask(v: Value) -> Value {
    match v {
        Value::String(s) if s.starts_with("sk-") || s.starts_with("nvapi") => {
            let masked = if s.len() > 12 {
                format!("{}...{}", &s[..8], &s[s.len() - 4..])
            } else {
                "***".to_string()
            };
            Value::String(masked)
        }
        other => other,
    }
}

/// Truncate dicts/lists to 10 entries and strings to keep payloads small (PS1 `short`).
fn short(v: Value) -> Value {
    short_at(v, 0)
}
fn short_at(v: Value, d: usize) -> Value {
    if d > 6 {
        return Value::String("[...]".to_string());
    }
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, x) in map.into_iter().take(10) {
                out.insert(k, short_at(x, d + 1));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .take(10)
                .map(|x| short_at(x, d + 1))
                .collect(),
        ),
        other => other,
    }
}

/// Export the live surface of every profile into backups/provider-surface/<stamp>/,
/// advance last-known, and write last-export.txt pointer. Mirrors PS1 EXPORT.
pub fn export(home: &Path) -> Result<()> {
    let surface = home.join("backups").join("provider-surface");
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let stamp_dir = surface.join(&stamp);
    std::fs::create_dir_all(&stamp_dir)?;

    let mut manifest = Vec::new();
    for p in PROFILES {
        let cfg = env::config_yaml(home, p);
        if !cfg.exists() {
            println!("  [skip] {} (no config.yaml)", p);
            continue;
        }
        let surface_val = extract_surface(&cfg)?;
        let out = stamp_dir.join(format!("{}.json", p));
        let pretty = serde_json::to_string_pretty(&surface_val)?;
        std::fs::write(&out, pretty)?;
        manifest.push(format!("{}  {}  {}", p, sha256_hex(&out)?, out.display()));
        println!("  [ok]   {} exported", p);
    }
    let manifest_path = stamp_dir.join("manifest.txt");
    std::fs::write(&manifest_path, manifest.join("\n"))?;
    println!("manifest: {}", manifest_path.display());

    // advance last-known + pointer (PS1 semantics: -Export IS the operator saying this is known-good)
    let lk = surface.join("last-known");
    std::fs::create_dir_all(&lk)?;
    for p in PROFILES {
        let f = stamp_dir.join(format!("{}.json", p));
        if f.exists() {
            std::fs::copy(&f, lk.join(format!("{}.json", p)))?;
        }
    }
    std::fs::write(surface.join("last-export.txt"), &stamp)?;
    println!("last-known advanced to {}; pointer written", stamp);
    Ok(())
}

fn sha256_hex(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Diff live surface against baseline (golden.json if present, else last-known/).
/// Returns Ok(()) when clean; exits 1 via process when drift found (mirrors PS1).
pub fn diff(home: &Path, explicit_baseline: Option<&Path>) -> Result<()> {
    let surface = home.join("backups").join("provider-surface");
    let golden = surface.join("golden.json");
    let lk = surface.join("last-known");
    let base: PathBuf = if let Some(b) = explicit_baseline {
        b.to_path_buf()
    } else if golden.exists() {
        golden
    } else if lk.is_dir() {
        lk
    } else {
        bail!("no baseline found — run `export` first (or pass -Baseline)");
    };
    let base_label = if base.file_name().map(|f| f.to_string_lossy().into_owned())
        == Some("golden.json".into())
    {
        "GOLDEN"
    } else {
        "last-known"
    };

    println!(
        "PROVIDER-DRIFT CHECK {}",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    println!("baseline: {} state", base_label);

    let mut drifted = 0usize;
    let mut drift_lines: Vec<String> = Vec::new();
    for p in PROFILES {
        let cfg = env::config_yaml(home, p);
        if !cfg.exists() {
            continue;
        }
        let live = extract_surface(&cfg)?;
        let basef = if base.is_dir() {
            base.join(format!("{}.json", p))
        } else {
            base.to_path_buf()
        };
        if !basef.exists() {
            continue;
        }
        let basev: Value = serde_json::from_str(&std::fs::read_to_string(&basef)?)
            .with_context(|| format!("parse baseline {}", basef.display()))?;
        let profile_drift: Vec<String> = SURFACE_KEYS
            .iter()
            .filter(|k| live.get(*k) != basev.get(*k))
            .map(|k| k.to_string())
            .collect();
        if !profile_drift.is_empty() {
            drifted += 1;
            for k in &profile_drift {
                let lv = live.get(k).cloned().unwrap_or(Value::Null);
                let bv = basev.get(k).cloned().unwrap_or(Value::Null);
                let line_live = format!("  {} {:<22} -> LIVE={}", p, k, trunc(&lv.to_string(), 90));
                let line_base = format!(
                    "  {:<10} {:<22}   BASE({})={}",
                    "",
                    "",
                    base_label,
                    trunc(&bv.to_string(), 90)
                );
                println!("{}", line_live);
                println!("{}", line_base);
                drift_lines.push(line_live);
                drift_lines.push(line_base);
            }
        }
    }

    if drifted == 0 {
        println!("  no provider-surface drift detected");
        Ok(())
    } else {
        println!();
        println!(
            "  [FAIL] DRIFT: {} profile(s) differ from baseline",
            drifted
        );
        eprintln!("DRIFT_DETAILS_START");
        for l in &drift_lines {
            eprintln!("{}", l);
        }
        eprintln!("DRIFT_DETAILS_END");
        std::process::exit(1);
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_redacts_api_keys() {
        assert_eq!(
            mask(Value::String("sk-abcdef1234567890".into())),
            Value::String("sk-abcde...7890".into())
        );
        assert_eq!(
            mask(Value::String("plain".into())),
            Value::String("plain".into())
        );
    }

    #[test]
    fn surface_keys_stable() {
        assert_eq!(SURFACE_KEYS.len(), 12);
        assert!(SURFACE_KEYS.contains(&"model.provider"));
        assert!(SURFACE_KEYS.contains(&"display.language"));
    }

    #[test]
    fn extract_from_minimal_yaml() {
        let p = std::env::temp_dir().join("hug-test-extract.yaml");
        std::fs::write(
            &p,
            "model:\n  provider: openrouter\n  default: x\n  api_key: sk-1234567890123456\n",
        )
        .unwrap();
        let v = extract_surface(&p).unwrap();
        assert_eq!(v["model.provider"], "openrouter");
        let masked = v["model.api_key"].as_str().unwrap();
        assert!(
            masked.starts_with("sk-1234"),
            "api key not masked: {}",
            masked
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn get_path_nested() {
        let v: Value = serde_json::from_str(r#"{"a":{"b":{"c":1}}}"#).unwrap();
        assert_eq!(get_path(&v, "a.b.c"), Value::from(1));
        assert_eq!(get_path(&v, "a.missing"), Value::Null);
    }
}
