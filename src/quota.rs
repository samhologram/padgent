//! Read-only, bounded quota observations from Codex session logs.
use serde_json::Value;
use std::{fs, io::{Read, Seek, SeekFrom}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use crate::model::Core;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quota {
    pub provider: String,
    pub window: String,
    pub remaining: Option<f64>,
    pub reset: Option<i64>,
    pub reset_text: Option<String>,
    pub updated: Option<String>,
    pub source: String,
}

/// Parse an individual Codex JSONL event. Unknown values never imply full allowance.
pub fn parse_codex_event(event: &Value) -> Vec<Quota> {
    let Some(limits) = event.get("payload").and_then(|p| p.get("rate_limits")).or_else(|| event.get("rate_limits")) else { return vec![] };
    let Some(obj) = limits.as_object() else { return vec![] };
    let updated = event.get("timestamp").and_then(Value::as_str).map(str::to_owned);
    let model = obj.get("limit_name").or_else(|| obj.get("limit_id")).and_then(Value::as_str).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("codex"));
    let mut out = Vec::new();
    for (key, value) in obj {
        let Some(used) = value.get("used_percent").and_then(Value::as_f64).filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) else { continue };
        let minutes = value.get("window_minutes").and_then(Value::as_i64);
        let window = match minutes {
            Some(10080) => "Weekly".to_string(),
            Some(m) if m > 0 && m <= 360 => "Session".to_string(),
            Some(m) if m > 0 => format!("{m} min"),
            _ => key.replace('_', " "),
        };
        let window = model.map(|name| format!("{window} · {name}")).unwrap_or(window);
        out.push(Quota { provider: "Codex".to_string(), window, remaining: Some(100.0 - used), reset: value.get("resets_at").and_then(Value::as_i64), reset_text: None, updated: updated.clone(), source: "Codex session log".into() });
    }
    out
}

fn recent_files(dir: &Path, depth: usize, files: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    if depth > 4 { return; }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() { directories.push(entry.path()); }
        else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) { files.push((modified, entry.path())); }
        }
    }
    // Codex organizes sessions by year/month/day. Bound traversal to recent date folders.
    directories.sort_unstable_by(|a, b| b.cmp(a));
    for path in directories.into_iter().take(3) { recent_files(&path, depth + 1, files); }
}

fn read_latest() -> Vec<Quota> {
    let Some(home) = dirs::home_dir() else { return vec![] };
    let root = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex")).join("sessions");
    let mut files = Vec::new();
    recent_files(&root, 0, &mut files);
    files.sort_unstable_by(|a,b| b.0.cmp(&a.0));
    let mut latest: std::collections::BTreeMap<(String, String), Quota> = std::collections::BTreeMap::new();
    for (_, path) in files.into_iter().take(12) {
        let Ok(mut file) = fs::File::open(path) else { continue };
        let Ok(len) = file.metadata().map(|m| m.len()) else { continue };
        let offset = len.saturating_sub(512 * 1024);
        if file.seek(SeekFrom::Start(offset)).is_err() { continue }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() { continue }
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().rev() {
            let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
            for q in parse_codex_event(&value) {
                let key = (q.provider.clone(), q.window.clone());
                match latest.get(&key) {
                    Some(previous) if previous.updated >= q.updated => {},
                    _ => { latest.insert(key, q); }
                }
            }
        }
    }
    latest.into_values().collect()
}

/// Decode only observed, well-formed snapshots. Observation times are never refreshed here.
pub fn parse_cache(data: &str) -> Vec<Quota> {
    let Ok(records) = serde_json::from_str::<Vec<Quota>>(data) else { return Vec::new() };
    records.into_iter().filter(|q| {
        !q.provider.trim().is_empty() && !q.window.trim().is_empty() && !q.source.trim().is_empty()
            && q.remaining.is_some_and(|v| v.is_finite() && (0.0..=100.0).contains(&v))
            && q.updated.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).is_some()
    }).collect()
}

pub fn start(core: Arc<Mutex<Core>>, wake: Arc<dyn Fn() + Send + Sync>) {
    std::thread::spawn(move || {
        let dir = crate::model::data_dir();
        let path = dir.join("quota-observations.json");
        if let Ok(data) = fs::read_to_string(&path) {
            let cached = parse_cache(&data);
            if let Ok(mut c) = core.lock() {
                for q in cached { observe(&mut c, q); }
            }
            wake();
        }
        let mut last_scan: Option<std::time::Instant> = None;
        let mut persisted: Option<Vec<Quota>> = None;
        loop {
            if last_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                let found = read_latest();
                if let Ok(mut c) = core.lock() {
                    let revision = c.revision;
                    for q in found { observe(&mut c, q); }
                    if c.revision != revision { wake(); }
                }
                last_scan = Some(std::time::Instant::now());
            }
            let Ok(snapshot) = core.lock().map(|c| c.quotas.clone()) else {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };
            if persisted.as_ref() != Some(&snapshot) {
                if let Ok(data) = serde_json::to_vec(&snapshot) {
                    let temp = dir.join("quota-observations.json.tmp");
                    if fs::create_dir_all(&dir).is_ok() && fs::write(&temp, data).is_ok() && fs::rename(&temp, &path).is_ok() {
                        persisted = Some(snapshot);
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

/// Compact, honest context for a reading on both displays.
pub fn detail(q: &Quota) -> String {
    if q.remaining.is_none() {
        return if q.updated.is_some() && !q.source.is_empty() { "Reading expired".into() } else { "No data".into() };
    }
    if !is_fresh(q, crate::model::now() as i64) { return "Reading expired".into(); }
    if let Some(reset) = &q.reset_text { return reset.clone(); }
    if let Some(reset) = q.reset {
        let seconds = reset - crate::model::now() as i64;
        if seconds > 0 { return format!("Reset in {}h", (seconds + 3599) / 3600); }
        return "Reset time passed".into();
    }
    if let Some(updated) = &q.updated { return format!("As of {}", updated.chars().take(10).collect::<String>()); }
    "Reset unknown".into()
}

/// Parse the usage summary exposed by Claude Desktop's Accessibility interface.
/// Percentages in this UI are usage, not remaining allowance.
pub fn parse_claude_usage(label: &str, observed_at: &str) -> Option<Quota> {
    let usage = label.strip_prefix("Usage:")?.trim();
    let (window, rest) = usage.split_once(':')?;
    let percent_end = rest.find('%')?;
    let number = rest[..percent_end].trim();
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit() || c == '.') { return None; }
    let used: f64 = number.parse().ok()?;
    let reset_text = rest[percent_end + 1..].trim().trim_start_matches(',').trim();
    parse_claude_window(window, used, reset_text, observed_at)
}

/// Interpret a named usage indicator from Claude Desktop's expanded usage panel.
pub fn parse_claude_window(window: &str, used: f64, reset_text: &str, observed_at: &str) -> Option<Quota> {
    if !used.is_finite() || !(0.0..=100.0).contains(&used) { return None; }
    let window = window.trim();
    let window = if window.eq_ignore_ascii_case("Weekly · all models") || window.eq_ignore_ascii_case("Weekly") { "Weekly" }
        else if window.eq_ignore_ascii_case("5-hour limit") || window.eq_ignore_ascii_case("Session · all models") || window.eq_ignore_ascii_case("Session") { "Session" }
        else if window.to_lowercase().starts_with("weekly · ") { window }
        else { return None };
    let reset_text = reset_text.trim();
    Some(Quota { provider: "Claude".into(), window: window.into(), remaining: Some(100.0 - used), reset: None,
        reset_text: (!reset_text.is_empty()).then(|| reset_text.to_owned()), updated: Some(observed_at.into()), source: "Claude Desktop".into() })
}

/// Merge an observation without disturbing other sources or older windows.
pub fn observe(core: &mut Core, observation: Quota) {
    if let Some(existing) = core.quotas.iter_mut().find(|q| q.provider == observation.provider && q.window == observation.window && q.source == observation.source) {
        let timestamp = |value: &Option<String>| value.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|t| t.timestamp_millis());
        if timestamp(&existing.updated) > timestamp(&observation.updated) { return; }
        if *existing == observation { return; }
        *existing = observation;
    } else { core.quotas.push(observation); }
    core.revision += 1;
}

#[cfg(test)]
mod tests {
    use super::{Quota, displayed_remaining, is_fresh, observe, parse_cache, parse_claude_usage, parse_claude_window, parse_codex_event, remaining_colour};
    use crate::model::{Core, Settings};

    #[test]
    fn codex_rate_limit_event_parses_generic_session_and_weekly_windows() {
        let event = serde_json::json!({
            "timestamp": "2026-11-11T15:30:00Z",
            "payload": {"rate_limits": {
                "limit_name": "codex",
                "primary": {"used_percent": 25.0, "window_minutes": 300, "resets_at": 1800000000},
                "secondary": {"used_percent": 80.0, "window_minutes": 10080, "resets_at": 1801000000}
            }}
        });

        let quotas = parse_codex_event(&event);
        assert_eq!(quotas.len(), 2);
        let session = quotas.iter().find(|q| q.window == "Session").unwrap();
        assert_eq!(session.provider, "Codex");
        assert_eq!(session.remaining, Some(75.0));
        assert_eq!(session.reset, Some(1800000000));
        assert_eq!(session.updated.as_deref(), Some("2026-11-11T15:30:00Z"));
        let weekly = quotas.iter().find(|q| q.window == "Weekly").unwrap();
        assert_eq!(weekly.remaining, Some(20.0));
    }

    #[test]
    fn codex_percentages_are_bounded_and_missing_values_do_not_create_quota() {
        let event = serde_json::json!({
            "rate_limits": {
                "limit_id": "codex",
                "zero": {"used_percent": 0.0, "window_minutes": 30},
                "full": {"used_percent": 100.0, "window_minutes": 10080},
                "negative": {"used_percent": -0.1, "window_minutes": 300},
                "over": {"used_percent": 100.1, "window_minutes": 300},
                "unknown": {"window_minutes": 300},
                "string": {"used_percent": "50", "window_minutes": 300}
            }
        });
        let quotas = parse_codex_event(&event);
        assert_eq!(quotas.len(), 2);
        assert!(quotas.iter().any(|q| q.window == "Session" && q.remaining == Some(100.0)));
        assert!(quotas.iter().any(|q| q.window == "Weekly" && q.remaining == Some(0.0)));
        assert!(parse_codex_event(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn unknown_or_nonpositive_codex_windows_keep_their_source_label() {
        let event = serde_json::json!({"rate_limits": {
            "custom_window": {"used_percent": 10.0, "window_minutes": 0},
            "other_window": {"used_percent": 20.0, "window_minutes": -5},
            "primary": {"used_percent": 30.0}
        }});
        let quotas = parse_codex_event(&event);
        assert_eq!(quotas.len(), 3);
        assert!(quotas.iter().any(|q| q.window == "custom window"));
        assert!(quotas.iter().any(|q| q.window == "other window"));
        assert!(quotas.iter().any(|q| q.window == "primary"));
    }

    #[test]
    fn freshness_bounds_reading_age_future_skew_and_reset() {
        let make = |updated: &str, reset, remaining| Quota {
            provider: "Codex".into(), window: "Session".into(), remaining, reset,
            reset_text: None, updated: Some(updated.into()), source: "test".into(),
        };
        let now = 1_800_000_000i64;
        assert!(is_fresh(&make("2027-01-15T07:45:00Z", None, Some(75.0)), now)); // age exactly 900 s
        assert!(!is_fresh(&make("2027-01-15T07:44:59Z", None, Some(75.0)), now));
        assert!(is_fresh(&make("2027-01-15T08:01:00Z", None, Some(75.0)), now)); // 60 s future skew
        assert!(!is_fresh(&make("2027-01-15T08:01:01Z", None, Some(75.0)), now));
        assert!(!is_fresh(&make("2027-01-15T08:00:00Z", Some(now), Some(75.0)), now)); // reset passed
        assert_eq!(displayed_remaining(&make("2027-01-15T07:44:59Z", None, Some(75.0)), now), None);
        assert_eq!(displayed_remaining(&make("2027-01-15T07:45:00Z", None, Some(75.0)), now), Some(75.0));
        assert_eq!(displayed_remaining(&make("2027-01-15T07:45:00Z", None, Some(0.0)), now), Some(0.0));
        assert_eq!(displayed_remaining(&make("2027-01-15T07:45:00Z", None, Some(100.0)), now), Some(100.0));
        for invalid in [None, Some(-0.01), Some(100.01), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(displayed_remaining(&make("2027-01-15T07:45:00Z", None, invalid), now), None);
        }
        assert!(!is_fresh(&make("not a timestamp", None, Some(50.0)), now));
    }

    #[test]
    fn remaining_colour_uses_inclusive_thresholds_and_rejects_out_of_range_values() {
        use crate::colour::Rgb;
        for (value, expected) in [
            (100.0, Rgb(48, 209, 88)),
            (75.0, Rgb(48, 209, 88)),
            (74.99, Rgb(255, 214, 10)),
            (50.0, Rgb(255, 214, 10)),
            (49.99, Rgb(255, 159, 10)),
            (25.0, Rgb(255, 159, 10)),
            (24.99, Rgb(255, 69, 58)),
            (0.0, Rgb(255, 69, 58)),
        ] {
            assert_eq!(remaining_colour(value), Some(expected), "remaining={value}");
        }
        for invalid in [-0.01, 100.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(remaining_colour(invalid), None, "invalid remaining={invalid}");
        }
    }

    #[test]
    fn observation_replaces_only_matching_source_and_newer_timestamp() {
        let mut core = Core::new(Settings::default());
        let make = |source: &str, updated: &str, remaining| Quota {
            provider: "Codex".into(), window: "Session".into(), remaining: Some(remaining), reset: None,
            reset_text: None, updated: Some(updated.into()), source: source.into(),
        };
        observe(&mut core, make("Codex session log", "2026-11-11T10:00:00Z", 80.0));
        observe(&mut core, make("Codex session log", "2026-11-11T09:00:00Z", 10.0));
        assert_eq!(core.quotas.len(), 1);
        assert_eq!(core.quotas[0].remaining, Some(80.0));
        observe(&mut core, make("Claude Desktop", "2026-11-11T09:00:00Z", 50.0));
        assert_eq!(core.quotas.len(), 2);
        observe(&mut core, make("Codex session log", "2026-11-11T11:00:00Z", 60.0));
        assert_eq!(core.quotas.len(), 2);
        assert_eq!(core.quotas.iter().find(|q| q.source == "Codex session log").unwrap().remaining, Some(60.0));
    }

    #[test]
    fn claude_usage_parser_converts_used_percent_to_remaining_and_rejects_bad_values() {
        let q = parse_claude_usage("Usage: Weekly · all models: 23.5% · resets in 2 days", "2026-11-11T15:30:00Z").unwrap();
        assert_eq!(q.provider, "Claude");
        assert_eq!(q.window, "Weekly");
        assert_eq!(q.remaining, Some(76.5));
        assert_eq!(q.reset_text.as_deref(), Some("· resets in 2 days"));
        assert_eq!(q.updated.as_deref(), Some("2026-11-11T15:30:00Z"));
        assert!(parse_claude_usage("Usage: Session: -2%", "now").is_none());
        assert!(parse_claude_usage("Usage: Session: 101%", "now").is_none());
        assert!(parse_claude_usage("Session: 10%", "now").is_none());
    }

    #[test]
    fn claude_numeric_accessibility_indicators_map_usage_to_provider_windows() {
        let at = "2026-11-11T15:30:00Z";
        let session = parse_claude_window("5-hour limit", 37.0, "Resets in 4h", at).unwrap();
        assert_eq!((session.provider.as_str(), session.window.as_str()), ("Claude", "Session"));
        assert_eq!(session.remaining, Some(63.0));
        assert_eq!(session.reset_text.as_deref(), Some("Resets in 4h"));
        assert_eq!(session.updated.as_deref(), Some(at));

        let weekly = parse_claude_window("Weekly · all models", 58.0, "Resets Monday", at).unwrap();
        assert_eq!((weekly.provider.as_str(), weekly.window.as_str()), ("Claude", "Weekly"));
        assert_eq!(weekly.remaining, Some(42.0));
        let model = parse_claude_window("Weekly · Fable", 58.0, "", at).unwrap();
        assert_eq!(model.window, "Weekly · Fable");
        assert_eq!(parse_claude_window("Settings", 58.0, "", at), None);
        assert_eq!(parse_claude_window("5-hour limit", -1.0, "", at), None);
        assert_eq!(parse_claude_window("5-hour limit", f64::NAN, "", at), None);
    }

    #[test]
    fn quota_cache_keeps_valid_old_observations_and_discards_invalid_records() {
        let valid = Quota {
            provider: "Codex".into(), window: "Weekly".into(), remaining: Some(42.0), reset: None,
            reset_text: None, updated: Some("2020-01-01T00:00:00Z".into()), source: "Codex session log".into(),
        };
        let mut bad_remaining = valid.clone();
        bad_remaining.remaining = Some(101.0);
        let mut missing_remaining = valid.clone();
        missing_remaining.remaining = None;
        let mut bad_time = valid.clone();
        bad_time.updated = Some("yesterday".into());
        let mut bad_source = valid.clone();
        bad_source.source = "  ".into();
        let records = vec![valid.clone(), bad_remaining, missing_remaining, bad_time, bad_source];

        let serialized = serde_json::to_string(&records).unwrap();
        let restored = parse_cache(&serialized);
        assert_eq!(restored, vec![valid.clone()]);
        assert_eq!(restored[0].updated.as_deref(), Some("2020-01-01T00:00:00Z"));
        assert!(parse_cache("not json").is_empty());
    }
}

/// UTC observation time suitable for stored source records.
pub fn observed_now() -> String { chrono::Utc::now().to_rfc3339() }

/// A reading expires after 15 minutes or its reported reset, whichever comes first.
pub fn is_fresh(q: &Quota, now_epoch: i64) -> bool {
    let Some(updated) = q.updated.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else { return false };
    let age = now_epoch - updated.timestamp();
    (-60..=900).contains(&age) && q.reset.is_none_or(|reset| reset > now_epoch)
}

pub fn displayed_remaining(q: &Quota, now_epoch: i64) -> Option<f64> {
    if is_fresh(q, now_epoch) { q.remaining.filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) } else { None }
}

/// Semantic color of the remaining allowance; invalid readings have no color.
pub fn remaining_colour(value: f64) -> Option<crate::colour::Rgb> {
    use crate::colour::Rgb;
    if !value.is_finite() || !(0.0..=100.0).contains(&value) { return None; }
    Some(if value >= 75.0 { Rgb(48, 209, 88) }
        else if value >= 50.0 { Rgb(255, 214, 10) }
        else if value >= 25.0 { Rgb(255, 159, 10) }
        else { Rgb(255, 69, 58) })
}
