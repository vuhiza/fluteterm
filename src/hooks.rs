//! Claude Code hooks in `~/.claude/settings.json`: install / remove / detect.
//! Shared by the CLI (`fluteterm install|uninstall claude`), the settings window
//! and the command palette. Every write makes a timestamped backup first, and
//! only fluteterm's own entries are ever touched.

use std::path::PathBuf;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::util;

pub const EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("Notification", false),
    ("Stop", false),
    ("PreCompact", false),
    ("SessionEnd", false),
];

pub fn settings_path() -> PathBuf {
    util::home().join(".claude").join("settings.json")
}

/// A hook entry that runs our `… hook claude` command.
fn is_ours(entry: &Value) -> bool {
    entry["hooks"]
        .as_array()
        .map(|hs| {
            hs.iter().any(|h| {
                h["command"].as_str().map(|c| (c.contains("fluteterm") || c.contains("drpdown")) && c.contains("hook claude")).unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn read() -> anyhow::Result<Option<Value>> {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => Ok(Some(serde_json::from_str(&s).context("~/.claude/settings.json is not valid JSON")?)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Number of fluteterm hook entries currently installed.
pub fn count() -> usize {
    let Ok(Some(settings)) = read() else { return 0 };
    settings["hooks"]
        .as_object()
        .map(|h| h.values().filter_map(|v| v.as_array()).map(|l| l.iter().filter(|e| is_ours(e)).count()).sum())
        .unwrap_or(0)
}

pub fn installed() -> bool {
    count() > 0
}

fn backup() -> anyhow::Result<Option<PathBuf>> {
    let path = settings_path();
    if !path.exists() {
        return Ok(None);
    }
    let b = path.with_file_name(format!("settings.json.fluteterm-backup-{}", util::now_unix()));
    std::fs::copy(&path, &b)?;
    Ok(Some(b))
}

pub struct Report {
    pub changed: usize,
    pub backup: Option<PathBuf>,
    pub command: String,
}

/// Add (or refresh) our hooks. With `dry_run`, returns the resulting JSON instead.
pub fn install(dry_run: bool) -> anyhow::Result<(Report, Option<String>)> {
    let exe = std::env::current_exe()?.display().to_string();
    let command = format!("{} hook claude", util::shell_quote(&exe));
    let mut settings = read()?.unwrap_or_else(|| json!({}));
    if !settings.is_object() {
        bail!("~/.claude/settings.json is not a JSON object");
    }
    if !settings["hooks"].is_object() {
        settings["hooks"] = json!({});
    }
    for (event, has_matcher) in EVENTS {
        let list = settings["hooks"][*event].as_array().cloned().unwrap_or_default();
        let mut list: Vec<Value> = list.into_iter().filter(|e| !is_ours(e)).collect();
        let mut entry = json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]});
        if *has_matcher {
            entry["matcher"] = json!("*");
        }
        list.push(entry);
        settings["hooks"][*event] = Value::Array(list);
    }
    let out = serde_json::to_string_pretty(&settings)?;
    if dry_run {
        return Ok((Report { changed: EVENTS.len(), backup: None, command }, Some(out)));
    }
    let backup = backup()?;
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, out + "\n")?;
    Ok((Report { changed: EVENTS.len(), backup, command }, None))
}

/// Remove only our hook entries (other hooks are left exactly as they were).
pub fn uninstall() -> anyhow::Result<Report> {
    let Some(mut settings) = read()? else {
        return Ok(Report { changed: 0, backup: None, command: String::new() });
    };
    let mut removed = 0;
    if let Some(hooks) = settings["hooks"].as_object_mut() {
        for (_, v) in hooks.iter_mut() {
            if let Some(list) = v.as_array_mut() {
                let before = list.len();
                list.retain(|e| !is_ours(e));
                removed += before - list.len();
            }
        }
        hooks.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }
    if removed == 0 {
        return Ok(Report { changed: 0, backup: None, command: String::new() });
    }
    if settings["hooks"].as_object().map(|h| h.is_empty()).unwrap_or(false) {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    let backup = backup()?;
    std::fs::write(settings_path(), serde_json::to_string_pretty(&settings)? + "\n")?;
    Ok(Report { changed: removed, backup, command: String::new() })
}
