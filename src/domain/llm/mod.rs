//! The vendored AI-diagnostics LLM layer (copied from
//! oh-my-docker `llm/`, transport swapped `curl` → `ureq`). Prompt
//! builders and response parsers are verbatim; the request runs in
//! `spawn_blocking`.
use crate::domain::config::LlmConfig;
use crate::domain::docker::diagnostics::DiagnosticContext;
use std::time::Duration;

pub fn build_system_prompt() -> String {
    "You are a Docker container diagnostics expert. Analyze the provided container context and respond ONLY with valid JSON.\n\
    The JSON must have exactly two fields:\n\
    - \"analysis\": Your root-cause analysis. Identify the likely issue, explain what the logs and metrics indicate.\n\
    - \"playbook\": Step-by-step human-readable repair instructions. Number each step.\n\
    Use clear, actionable language. If the container is healthy, say so.\n\
    Keep each field concise but thorough.\n\
    Respond ONLY with the JSON object, no markdown backticks, no preamble."
        .to_string()
}

pub fn build_user_prompt(ctx: &DiagnosticContext) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!("Container ID: {}\n", ctx.container_id));
    prompt.push_str(&format!("Container Name: {}\n", ctx.container_name));
    prompt.push_str(&format!("Image: {}\n", ctx.image));
    prompt.push_str(&format!("State: {}\n", ctx.state));
    prompt.push_str(&format!("Status: {}\n", ctx.status));
    prompt.push_str(&format!("Command: {}\n", ctx.command));
    if let Some(code) = ctx.exit_code {
        prompt.push_str(&format!("Exit Code: {code}\n"));
    }
    if let Some(ref stats) = ctx.stats {
        prompt.push_str("\nResource Usage:\n");
        prompt.push_str(&format!("  CPU: {:.1}%\n", stats.cpu_percent));
        prompt.push_str(&format!(
            "  Memory: {} / {} ({:.1}%)\n",
            format_bytes(stats.memory_usage),
            format_bytes(stats.memory_limit),
            stats.memory_percent
        ));
        prompt.push_str(&format!(
            "  Network RX: {}  TX: {}\n",
            format_bytes(stats.net_rx),
            format_bytes(stats.net_tx)
        ));
        prompt.push_str(&format!("  PIDs: {}\n", stats.pids));
    }
    if !ctx.env_vars.is_empty() {
        prompt.push_str("\nEnvironment Variables:\n");
        for (k, v) in &ctx.env_vars {
            prompt.push_str(&format!("  {k}={}\n", mask_secret(k, v)));
        }
    }
    if !ctx.recent_logs.is_empty() {
        prompt.push_str("\nRecent Logs (last 100 lines):\n");
        for log in &ctx.recent_logs {
            prompt.push_str(&format!("{} {}\n", log.timestamp, log.message.trim()));
        }
    }
    prompt.push_str(
        "\nDiagnose the issue with this container and provide a JSON response with analysis and playbook fields.",
    );
    prompt
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub fn mask_secret(key: &str, value: &str) -> String {
    let lower = key.to_lowercase();
    if lower.contains("key")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("auth")
        || lower.contains("credential")
        || lower.ends_with("_url")
        || lower.ends_with("_uri")
        || lower.ends_with("_dsn")
    {
        "***REDACTED***".to_string()
    } else {
        value.to_string()
    }
}

/// Resolve the request URL from `base_url` (+ provider suffix) or the
/// provider's public default (vendored).
pub fn resolve_url(config: &LlmConfig) -> String {
    match config.base_url.as_deref().filter(|s| !s.is_empty()) {
        Some(base) => {
            let base = base.trim_end_matches('/');
            match config.provider.as_str() {
                "ollama" => format!("{base}/api/chat"),
                "openai" => format!("{base}/chat/completions"),
                "anthropic" => format!("{base}/messages"),
                _ => base.to_string(),
            }
        }
        None => match config.provider.as_str() {
            "ollama" => "http://localhost:11434/api/chat".to_string(),
            "openai" => "https://api.openai.com/v1/chat/completions".to_string(),
            "anthropic" => "https://api.anthropic.com/v1/messages".to_string(),
            _ => String::new(),
        },
    }
}

/// Provider auth headers (ruling 2). Empty when no key is configured.
pub fn auth_headers(config: &LlmConfig) -> Vec<(String, String)> {
    let Some(key) = config.api_key.as_deref().filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    match config.provider.as_str() {
        "openai" => vec![("Authorization".to_string(), format!("Bearer {key}"))],
        "anthropic" => vec![
            ("x-api-key".to_string(), key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ],
        // ollama is unauthenticated by default.
        _ => Vec::new(),
    }
}

pub fn build_ollama_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "format": "json"
    })
}

pub fn build_openai_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    })
}

pub fn build_anthropic_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "stream": false,
        "system": system,
        "messages": [{"role": "user", "content": user}]
    })
}

/// Run one diagnostics request end-to-end (buffered, ruling 3). The
/// blocking `ureq` call runs in `spawn_blocking` so a slow provider
/// never stalls the tokio worker.
pub async fn request_diagnostics(
    config: &LlmConfig,
    ctx: &DiagnosticContext,
) -> anyhow::Result<(String, String)> {
    let system = build_system_prompt();
    let user = build_user_prompt(ctx);
    let body = match config.provider.as_str() {
        "ollama" => build_ollama_body(&config.model, &system, &user),
        "openai" => build_openai_body(&config.model, &system, &user),
        "anthropic" => build_anthropic_body(&config.model, &system, &user),
        other => anyhow::bail!("unknown LLM provider: {other}"),
    };
    let url = resolve_url(config);
    let headers = auth_headers(config);

    let raw = tokio::task::spawn_blocking(move || post_json(&url, &headers, &body))
        .await
        .map_err(|e| anyhow::anyhow!("llm task join: {e}"))??;

    parse_diagnostic_json(&raw).map_err(|e| anyhow::anyhow!("llm response: {e}"))
}

/// The blocking POST. `http_status_as_error(false)` keeps the provider's
/// error body readable instead of collapsing it into a ureq error.
fn post_json(
    url: &str,
    headers: &[(String, String)],
    body: &serde_json::Value,
) -> anyhow::Result<String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build();
    let agent = config.new_agent();
    let mut req = agent.post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send_json(body)?;
    let status = resp.status().as_u16();
    // `Body::read_to_string` caps the read at ureq's 10 MiB `MAX_BODY_SIZE`
    // (and is lossy-utf8); exceeding it yields `Error::BodyExceedsLimit`.
    let text = resp.into_body().read_to_string()?;
    if !(200..300).contains(&status) {
        anyhow::bail!("HTTP {status}: {}", truncate(&text, 300));
    }
    Ok(text)
}

/// First `max` chars of `s` (the error-body cap; UTF-8 safe).
fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{t}…")
    } else {
        t
    }
}

/// Parse a provider's raw response body into `(analysis, playbook)`.
/// Vendored from tvdocker `llm/mod.rs:214`; made `pub` so the pure
/// parser is part of the module surface (Task 6 calls it internally).
pub fn parse_diagnostic_json(raw: &str) -> Result<(String, String), String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("JSON parse error: {}", e))?;

    // Try to extract analysis/playbook from the root object (Ollama with format=json)
    if let Some((a, p)) = extract_analysis_playbook(&v) {
        return Ok((a, p));
    }

    // Try OpenAI / Anthropic wrapper: extract the inner content string, parse it as JSON
    let content_str = extract_content_string(&v);
    if let Some(content) = content_str {
        if let Ok(inner) = serde_json::from_str::<serde_json::Value>(content)
            && let Some((a, p)) = extract_analysis_playbook(&inner)
        {
            return Ok((a, p));
        }
        // Content is plain text (not JSON) — return as analysis with a note
        return Ok((
            content.to_string(),
            "Repair playbook not available — check the analysis above.".to_string(),
        ));
    }

    // Fallback: return the raw response
    Ok((
        raw.to_string(),
        "Could not extract structured diagnostic data from the response.".to_string(),
    ))
}

fn extract_analysis_playbook(v: &serde_json::Value) -> Option<(String, String)> {
    let map = v.as_object()?;
    let analysis = map.get("analysis")?.as_str()?;
    let playbook = map
        .get("playbook")
        .map(|p| match p {
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            serde_json::Value::String(s) => s.clone(),
            _ => p.to_string(),
        })
        .unwrap_or_default();
    Some((analysis.to_string(), playbook))
}

fn extract_content_string(v: &serde_json::Value) -> Option<&str> {
    let map = v.as_object()?;

    // OpenAI: choices[0].message.content
    map.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|m| m.get("message"))
        .and_then(|c| c.get("content"))
        .and_then(|s| s.as_str())
        // Ollama: message.content
        .or_else(|| {
            map.get("message")
                .and_then(|m| m.get("content"))
                .and_then(|s| s.as_str())
        })
        // Anthropic: content[0].text
        .or_else(|| {
            map.get("content")
                .and_then(|c| c.get(0))
                .and_then(|t| t.get("text"))
                .and_then(|s| s.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::config::LlmConfig;
    use crate::domain::docker::logs::LogLine;
    use crate::domain::docker::statistics::StatEntry;

    fn ctx() -> DiagnosticContext {
        DiagnosticContext {
            container_id: "abc123".into(),
            container_name: "web".into(),
            state: "Running".into(),
            exit_code: None,
            recent_logs: vec![LogLine {
                timestamp: "2026-10-12T10:00:00Z".into(),
                message: "panic: boom".into(),
            }],
            stats: Some(StatEntry {
                id: "abc123".into(),
                name: "web".into(),
                cpu_percent: 12.5,
                memory_usage: 1024 * 1024,
                memory_limit: 1024 * 1024 * 1024,
                memory_percent: 0.1,
                net_rx: 70,
                net_tx: 0,
                block_read: 0,
                block_write: 0,
                pids: 3,
            }),
            env_vars: vec![
                ("PATH".into(), "/usr/bin".into()),
                ("API_KEY".into(), "sk-live-123".into()),
            ],
            command: "nginx -g daemon off;".into(),
            image: "nginx:latest".into(),
            status: "Running".into(),
        }
    }

    /// Review Focus #1: a secret-looking env value must never leave the
    /// host in the prompt.
    #[test]
    fn mask_secret_redacts_secret_looking_keys() {
        for key in [
            "API_KEY",
            "db_password",
            "AUTH_TOKEN",
            "my_secret",
            "credential",
        ] {
            assert_eq!(mask_secret(key, "sk-live-123"), "***REDACTED***", "{key}");
        }
        assert_eq!(mask_secret("PATH", "/usr/bin"), "/usr/bin");
    }

    /// Review fix #1: connection-string keys carry credentials in the
    /// value (`postgres://user:pw@host`), so they must be redacted too.
    #[test]
    fn mask_secret_redacts_url_style_secret_keys() {
        for key in ["DATABASE_URL", "MONGODB_URI", "REDIS_URL", "SENTRY_DSN"] {
            assert_eq!(
                mask_secret(key, "postgres://admin:hunter2@db:5432/app"),
                "***REDACTED***",
                "{key}"
            );
        }
        // Suffix match, not substring: a key that merely contains "url"
        // stays intact.
        assert_eq!(mask_secret("CURL_OPTIONS", "-sSL"), "-sSL");
    }

    /// Review Focus #3: the prompt carries the identity, stats and logs.
    #[test]
    fn build_user_prompt_carries_identity_and_logs() {
        let p = build_user_prompt(&ctx());
        assert!(p.contains("Container ID: abc123"), "{p}");
        assert!(p.contains("Image: nginx:latest"), "{p}");
        assert!(
            p.contains("panic: boom"),
            "the log tail is the payload: {p}"
        );
        assert!(p.contains("CPU: 12.5%"), "{p}");
        assert!(
            p.contains("***REDACTED***"),
            "secret masked in the prompt: {p}"
        );
        assert!(!p.contains("sk-live-123"), "never leak the key: {p}");
    }

    #[test]
    fn format_bytes_scales_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn the_system_prompt_demands_json() {
        let s = build_system_prompt();
        assert!(s.contains("\"analysis\""), "{s}");
        assert!(s.contains("\"playbook\""), "{s}");
    }

    /// Root-object form (ollama with `format:"json"`), array playbook.
    #[test]
    fn parses_the_root_object_form() {
        let raw = r#"{"analysis":"disk full","playbook":["1. prune","2. restart"]}"#;
        let (a, p) = parse_diagnostic_json(raw).expect("parse");
        assert_eq!(a, "disk full");
        assert_eq!(p, "1. prune\n2. restart");
    }

    /// OpenAI envelope: the JSON lives inside choices[0].message.content.
    #[test]
    fn parses_the_openai_envelope() {
        let raw =
            r#"{"choices":[{"message":{"content":"{\"analysis\":\"a\",\"playbook\":\"b\"}"}}]}"#;
        assert_eq!(
            parse_diagnostic_json(raw).expect("parse"),
            ("a".to_string(), "b".to_string())
        );
    }

    /// Anthropic envelope: content[0].text.
    #[test]
    fn parses_the_anthropic_envelope() {
        let raw = r#"{"content":[{"text":"{\"analysis\":\"a\",\"playbook\":\"b\"}"}]}"#;
        assert_eq!(
            parse_diagnostic_json(raw).expect("parse"),
            ("a".to_string(), "b".to_string())
        );
    }

    /// A plain-text answer is not an error — it becomes the analysis.
    /// The provider still wraps it in a valid envelope (a bare non-JSON
    /// body is unparseable, per `malformed_json_is_an_error`).
    #[test]
    fn plain_text_becomes_the_analysis() {
        let raw = r#"{"choices":[{"message":{"content":"everything looks fine"}}]}"#;
        let (a, p) = parse_diagnostic_json(raw).expect("parse");
        assert_eq!(a, "everything looks fine");
        assert!(p.contains("playbook not available"), "{p}");
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_diagnostic_json("{not json").is_err());
    }

    fn cfg(provider: &str, base_url: Option<&str>, api_key: Option<&str>) -> LlmConfig {
        LlmConfig {
            provider: provider.into(),
            model: "m".into(),
            base_url: base_url.map(str::to_string),
            api_key: api_key.map(str::to_string),
        }
    }

    /// Review Focus #2: the key IS sent (tvdocker never sent it).
    #[test]
    fn auth_headers_cover_each_provider() {
        assert!(auth_headers(&cfg("ollama", None, Some("k"))).is_empty());
        assert_eq!(
            auth_headers(&cfg("openai", None, Some("sk-1"))),
            vec![("Authorization".to_string(), "Bearer sk-1".to_string())]
        );
        let anth = auth_headers(&cfg("anthropic", None, Some("sk-a")));
        assert!(anth.contains(&("x-api-key".to_string(), "sk-a".to_string())));
        assert!(anth.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())));
        // No key configured ⇒ no auth header (and no empty-value header).
        assert!(auth_headers(&cfg("openai", None, None)).is_empty());
    }

    #[test]
    fn resolve_url_uses_the_base_or_the_provider_default() {
        assert_eq!(
            resolve_url(&cfg("ollama", None, None)),
            "http://localhost:11434/api/chat"
        );
        assert_eq!(
            resolve_url(&cfg("openai", None, None)),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_url(&cfg("anthropic", Some("https://proxy/"), None)),
            "https://proxy/messages"
        );
    }

    #[test]
    fn provider_bodies_carry_the_model_and_prompts() {
        let b = build_ollama_body("llama3", "S", "U");
        assert_eq!(b["model"], "llama3");
        assert_eq!(b["stream"], false);
        assert_eq!(b["messages"][0]["content"], "S");
        let o = build_openai_body("gpt", "S", "U");
        assert_eq!(o["max_tokens"], 4096);
        let a = build_anthropic_body("claude", "S", "U");
        assert_eq!(a["system"], "S");
        assert_eq!(a["stream"], false);
    }

    #[test]
    fn truncate_caps_error_bodies() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 3), "abc…");
    }

    /// Live end-to-end probe (needs a reachable provider). Opt in with
    ///   TVDOCKER_LLM_LIVE=1 TVDOCKER_LLM_PROVIDER=ollama \
    ///     TVDOCKER_LLM_BASE_URL=http://localhost:11434 \
    ///     ./dcargo test --locked -- --ignored diagnostics_llm_live
    /// A configured openai/anthropic run also needs TVDOCKER_LLM_API_KEY.
    #[tokio::test]
    #[ignore = "opt-in: TVDOCKER_LLM_LIVE=1 + a reachable provider"]
    async fn diagnostics_llm_live() {
        if std::env::var("TVDOCKER_LLM_LIVE").as_deref() != Ok("1") {
            return; // silently skip: no provider configured
        }
        let config = LlmConfig {
            provider: std::env::var("TVDOCKER_LLM_PROVIDER").unwrap_or_else(|_| "ollama".into()),
            model: std::env::var("TVDOCKER_LLM_MODEL").unwrap_or_else(|_| "llama3".into()),
            base_url: std::env::var("TVDOCKER_LLM_BASE_URL").ok(),
            api_key: std::env::var("TVDOCKER_LLM_API_KEY").ok(),
        };
        let ctx = DiagnosticContext {
            container_id: "smoke".into(),
            container_name: "smoke".into(),
            state: "Exited".into(),
            image: "busybox".into(),
            status: "Exited (1)".into(),
            recent_logs: vec![LogLine {
                timestamp: "2026-10-12T10:00:00Z".into(),
                message: "boom".into(),
            }],
            ..Default::default()
        };
        let (analysis, _playbook) = request_diagnostics(&config, &ctx).await.expect("live llm");
        assert!(!analysis.trim().is_empty(), "an analysis came back");
    }
}
