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

    // Refresh the aggregate golden.json (the shape the PS1 exporter writes too)
    // so `--baseline golden` stays current; the previous one is kept as a .bak
    // rather than dropped.
    let golden_path = surface.join("golden.json");
    if golden_path.exists() {
        let _ = std::fs::copy(
            &golden_path,
            surface.join(format!("golden.json.bak-{}", stamp)),
        );
    }
    let mut agg = serde_json::Map::new();
    for p in PROFILES {
        let f = stamp_dir.join(format!("{}.json", p));
        if f.exists() {
            if let Ok(v) = read_json_file(&f) {
                agg.insert(p.to_string(), v);
            }
        }
    }
    let n_profiles = agg.len();
    std::fs::write(
        &golden_path,
        serde_json::to_string_pretty(&Value::Object(agg))?,
    )?;
    println!("golden.json refreshed ({} profiles aggregated)", n_profiles);
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

    let lines = compare_against(home, &base);
    for l in &lines {
        println!("{}", l);
    }

    let drifted = count_drift_lines(&lines);
    let unreadable = lines
        .iter()
        .filter(|l| l.contains(" baseline UNREADABLE"))
        .count();
    if drifted == 0 && unreadable == 0 {
        println!("  no provider-surface drift detected");
        Ok(())
    } else {
        println!();
        if drifted > 0 {
            println!(
                "  [FAIL] DRIFT: {} profile(s) differ from baseline",
                drifted
            );
        }
        if unreadable > 0 {
            println!(
                "  [FAIL] {} baseline file(s) UNREADABLE - drift check incomplete",
                unreadable
            );
        }
        eprintln!("DRIFT_DETAILS_START");
        for l in &lines {
            eprintln!("{}", l);
        }
        eprintln!("DRIFT_DETAILS_END");
        std::process::exit(1);
    }
}

/// Count how many profiles drifted from the shared compare walk output.
pub fn count_drift_lines(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|l| l.starts_with("  ") && l.contains(" drifted vs "))
        .count()
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

/// Read a JSON file as text, tolerating the UTF-8 BOM and CRLF that PowerShell's
/// `ConvertTo-Json` leaves behind. The PS1 exporters write `last-known/*.json`
/// with a BOM; `serde_json::from_str` fails on a leading BOM, and the old code
/// answered that failure with a silent `continue` — a fail-OPEN drift check that
/// reports "no drift" when the baseline simply could not be read.
pub fn read_json_file(p: &Path) -> Result<Value> {
    let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str(text).with_context(|| format!("parse json {}", p.display()))
}

/// Resolve one profile's baseline surface from a baseline path.
///
/// Three shapes are understood, because two different writers produce them:
///   * a **directory** of per-profile files — `last-known/<profile>.json`
///   * an **aggregate-by-profile** file     — `golden.json` = `{profile: {surface}}`
///   * a **flat single-profile** file       — an explicit `--baseline <path>`
///
/// The previous code treated *every* file baseline as the flat shape, so a
/// `golden.json` aggregate compared each profile against a wrapper object and
/// reported every surface key as drift with `BASE=null`.
///
/// `Ok(None)` = no baseline for this profile yet (not an error).
pub fn baseline_surface(base: &Path, profile: &str) -> Result<Option<Value>> {
    if base.is_dir() {
        let f = base.join(format!("{}.json", profile));
        if !f.exists() {
            return Ok(None);
        }
        return read_json_file(&f).map(Some);
    }
    if !base.exists() {
        return Ok(None);
    }
    let whole = read_json_file(base)?;
    // A flat file carries the surface keys at the top level; an aggregate carries
    // one object per profile. Decide by shape, never by guessing from the name.
    if whole.get("model.provider").is_some() {
        return Ok(Some(whole));
    }
    // aggregate-by-profile
    if let Some(sub) = whole.get(profile) {
        if sub.is_object() {
            return Ok(Some(sub.clone()));
        }
    }
    // Aggregate that does not list this profile: nothing to compare against yet.
    Ok(None)
}

/// Human label for the baseline a profile was compared against.
fn baseline_label(base: &Path) -> String {
    if base.is_dir() {
        format!(
            "{}/",
            base.file_name().unwrap_or_default().to_string_lossy()
        )
    } else {
        base.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

/// Shared drift-walk: compare every present profile's live surface against a
/// baseline (a directory of per-profile json, an aggregate-by-profile file, or a
/// flat single-profile file), returning per-profile drift lines.
/// Used by `diff`, `preflight` summary and `verify`.
///
/// A baseline that exists but cannot be parsed is reported as an explicit
/// `baseline UNREADABLE` line instead of being silently skipped, so a broken
/// baseline can never read as "no drift".
pub fn compare_against(home: &Path, base: &Path) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for p in PROFILES {
        let cfg = env::config_yaml(home, p);
        if !cfg.exists() {
            continue;
        }
        let Ok(live) = extract_surface(&cfg) else {
            continue;
        };
        match baseline_surface(base, p) {
            Ok(None) => continue,
            Ok(Some(basev)) => {
                let profile_drift: Vec<&str> = SURFACE_KEYS
                    .iter()
                    .filter(|k| live.get(*k) != basev.get(*k))
                    .copied()
                    .collect();
                if profile_drift.is_empty() {
                    continue;
                }
                let label = baseline_label(base);
                lines.push(format!("  {} drifted vs {}", p, label));
                for k in profile_drift {
                    lines.push(format!(
                        "  {:<22} -> LIVE={}  BASE={}",
                        k,
                        trunc(&live.get(k).cloned().unwrap_or(Value::Null).to_string(), 80),
                        trunc(
                            &basev.get(k).cloned().unwrap_or(Value::Null).to_string(),
                            80
                        )
                    ));
                }
            }
            Err(e) => lines.push(format!("  ! {} baseline UNREADABLE: {}", p, e)),
        }
    }
    lines
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

    #[test]
    fn compare_against_detects_drift() {
        // fake home with one profile whose model.provider differs from baseline
        let home = std::env::temp_dir().join("hug-test-home");
        let prof = home.join("profiles").join("elevate");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(
            prof.join("config.yaml"),
            "model:\n  provider: openrouter\n  default: x\n  api_key: sk-1234567890123456\n",
        )
        .unwrap();
        let base = home.join("base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("elevate.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "model.provider": "anthropic",
                "model.default": "x",
                "model.base_url": null,
                "model.api_key": "sk-1234567890123456",
                "auxiliary": null,
                "fallback_providers": null,
                "fallback_model": null,
                "delegation": null,
                "providers": null,
                "moa": null,
                "display.language": null,
                "stt.language": null,
            }))
            .unwrap(),
        )
        .unwrap();

        let lines = compare_against(&home, &base);
        assert!(
            lines.iter().any(|l| l.contains("elevate drifted vs")),
            "expected elevate drift line, got {:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l.contains("model.provider")),
            "expected key-level line, got {:?}",
            lines
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn read_json_file_strips_bom_and_crlf() {
        // the PS1 exporter writes a UTF-8 BOM + CRLF; serde_json alone fails on it
        let p = std::env::temp_dir().join("hug-test-bom.json");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"{\r\n  \"model.provider\": \"openrouter\"\r\n}\r\n");
        std::fs::write(&p, bytes).unwrap();
        let v = read_json_file(&p).unwrap();
        assert_eq!(v["model.provider"], "openrouter");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn baseline_surface_understands_aggregate_golden() {
        // golden.json is an aggregate-by-profile map, NOT one flat surface
        let home = std::env::temp_dir().join("hug-test-aggregate");
        let _ = std::fs::create_dir_all(&home);
        let golden = home.join("golden.json");
        std::fs::write(
            &golden,
            serde_json::to_string_pretty(&serde_json::json!({
                "elevate": {"model.provider": "openrouter", "model.default": "claude"},
                "mcpvot": {"model.provider": "nous", "model.default": "stepfun"}
            }))
            .unwrap(),
        )
        .unwrap();
        let elev = baseline_surface(&golden, "elevate").unwrap().unwrap();
        assert_eq!(elev["model.provider"], "openrouter");
        assert_eq!(elev["model.default"], "claude");
        // a profile absent from the aggregate is "no baseline", not a drift storm
        assert!(baseline_surface(&golden, "vot").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn baseline_surface_flat_file_is_used_verbatim() {
        let p = std::env::temp_dir().join("hug-test-flat.json");
        std::fs::write(
            &p,
            serde_json::to_string_pretty(&serde_json::json!({
                "model.provider": "openrouter",
                "model.default": "claude",
                "model.base_url": null,
                "model.api_key": null,
                "auxiliary": null,
                "fallback_providers": null,
                "fallback_model": null,
                "delegation": null,
                "providers": null,
                "moa": null,
                "display.language": null,
                "stt.language": null
            }))
            .unwrap(),
        )
        .unwrap();
        let v = baseline_surface(&p, "elevate").unwrap().unwrap();
        assert_eq!(v["model.provider"], "openrouter");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn compare_against_flags_unreadable_baseline() {
        // an existing baseline that will not parse must NOT read as "no drift"
        let home = std::env::temp_dir().join("hug-test-unreadable");
        let prof = home.join("profiles").join("elevate");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(
            prof.join("config.yaml"),
            "model:\n  provider: openrouter\n  default: x\n  api_key: sk-1234567890\n",
        )
        .unwrap();
        let base = home.join("base");
        std::fs::create_dir_all(&base).unwrap();
        let mut junk = vec![0xEF, 0xBB, 0xBF];
        junk.extend_from_slice(b"{not json at all}");
        std::fs::write(base.join("elevate.json"), junk).unwrap();
        let lines = compare_against(&home, &base);
        assert!(
            lines.iter().any(|l| l.contains("baseline UNREADABLE")),
            "expected an UNREADABLE line, got {:?}",
            lines
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
