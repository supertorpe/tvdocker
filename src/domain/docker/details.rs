//! Container inspect → the Details view's lines ("Parsed
//! inspect view"; the fetch + formatter lands in Task 4, adapted from
//! oh-my-docker's `ui/container_details.rs`).

use anyhow::Result;
use bollard::Docker;
use chrono::TimeZone;
use serde_json::Value;

/// One rendered inspect view: the pure formatter's
/// output plus the name for the window title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Details {
    /// Container name (leading `/` trimmed).
    pub name: String,
    /// Pre-rendered lines (`build_details` output).
    pub lines: Vec<String>,
}

pub async fn inspect_container(docker: &Docker, id: &str) -> Result<Details> {
    let inspect = docker.inspect_container(id, None).await?;
    let value = serde_json::to_value(&inspect)?;
    let name = value
        .get("Name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();
    Ok(Details {
        name,
        lines: build_details(&value),
    })
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |acc, key| acc.get(key))
}

fn text(v: &Value, path: &[&str]) -> String {
    at(v, path)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A section: header line (one leading space) + two-space-indented
/// values; empty sections vanish (vendored behavior). The indentation
/// is our own — ratatui panel styling has no direct port.
fn push_section(out: &mut Vec<String>, header: &str, lines: Vec<String>) {
    if lines.is_empty() {
        return;
    }
    out.push(format!(" {header}"));
    for line in lines {
        out.push(format!("  {line}"));
    }
}

fn one(s: String) -> Vec<String> {
    if s.is_empty() { Vec::new() } else { vec![s] }
}

fn cmd(v: &Value) -> Vec<String> {
    let parts: Vec<String> = at(v, &["Config", "Cmd"])
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    one(parts.join(" "))
}

fn status(v: &Value) -> Vec<String> {
    let s = text(v, &["State", "Status"]);
    one(if s.is_empty() { text(v, &["State"]) } else { s })
}

/// `Created` arrives as a unix-epoch STRING on most daemons; render it
/// human-readable when we can, raw otherwise (never panic).
fn created(v: &Value) -> Vec<String> {
    let Some(raw) = at(v, &["Created"]) else {
        return Vec::new();
    };
    if let Some(secs) = raw.as_i64() {
        return one(chrono::Local
            .timestamp_opt(secs, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| secs.to_string()));
    }
    let s = raw.as_str().unwrap_or_default();
    if s.is_empty() {
        return Vec::new();
    }
    one(chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| s.to_string()))
}

fn compose(v: &Value) -> Vec<String> {
    let Some(labels) = at(v, &["Config", "Labels"]).and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, prefix) in [
        ("com.docker.compose.project", "Project"),
        ("com.docker.compose.service", "Service"),
        ("com.docker.compose.project.config_files", "Config"),
    ] {
        if let Some(val) = labels
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.push(format!("{prefix}: {val}"));
        }
    }
    out
}

fn env(v: &Value) -> Vec<String> {
    at(v, &["Config", "Env"])
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn mounts(v: &Value) -> Vec<String> {
    at(v, &["Mounts"])
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let src = m.get("Source").and_then(Value::as_str).unwrap_or_default();
                    let dst = m
                        .get("Destination")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if src.is_empty() || dst.is_empty() {
                        None
                    } else {
                        Some(format!("{src} → {dst}"))
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn networks(v: &Value) -> Vec<String> {
    let Some(all) = at(v, &["NetworkSettings", "Networks"]).and_then(Value::as_object) else {
        return Vec::new();
    };
    all.iter()
        .map(|(name, cfg)| {
            match cfg
                .get("IPAddress")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(ip) => format!("{name} ({ip})"),
                None => name.clone(),
            }
        })
        .collect()
}

fn ports(v: &Value) -> Vec<String> {
    let Some(all) = at(v, &["NetworkSettings", "Ports"]).and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, bindings) in all {
        match bindings.as_array().filter(|b| !b.is_empty()) {
            Some(list) => {
                for b in list {
                    let host = b.get("HostIp").and_then(Value::as_str).unwrap_or_default();
                    let hp = b
                        .get("HostPort")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let prefix = if host.is_empty() || host == "::" {
                        hp.to_string()
                    } else {
                        format!("{host}:{hp}")
                    };
                    out.push(format!("{prefix} → {key}"));
                }
            }
            None => out.push(format!("{key} (unpublished)")),
        }
    }
    out
}

fn labels(v: &Value) -> Vec<String> {
    at(v, &["Config", "Labels"])
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, val)| format!("{k}={val}")).collect())
        .unwrap_or_default()
}

/// Render the inspect view's lines — section ORDER vendored from
/// oh-my-docker's `build_text` (Name, Image, Command, Status, Created,
/// blank, COMPOSE, ENVIRONMENT, VOLUMES, NETWORKS, PORTS, LABELS);
/// empty sections omitted, trailing blanks trimmed.
pub fn build_details(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let meta = out.len();
    push_section(
        &mut out,
        "Name",
        one(text(v, &["Name"]).trim_start_matches('/').to_string()),
    );
    push_section(&mut out, "Image", one(text(v, &["Config", "Image"])));
    push_section(&mut out, "Command", cmd(v));
    push_section(&mut out, "Status", status(v));
    push_section(&mut out, "Created", created(v));
    if out.len() > meta {
        out.push(String::new()); // blank after the metadata block
    }
    push_section(&mut out, "COMPOSE", compose(v));
    push_section(&mut out, "ENVIRONMENT", env(v));
    push_section(&mut out, "VOLUMES", mounts(v));
    push_section(&mut out, "NETWORKS", networks(v));
    push_section(&mut out, "PORTS", ports(v));
    push_section(&mut out, "LABELS", labels(v));
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}
