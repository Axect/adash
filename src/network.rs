//! Provider reachability (the network or VPN may come up after adash and the agents start).
//! Probes the hosts the agents talk to, the same way they connect (their proxy variables, if
//! any). Hosts:
//! - anthropic `https://api.anthropic.com/v1/models` (omp anthropic, claude)
//! - openai `https://chatgpt.com/backend-api/` (omp `openai-codex` and codex use the ChatGPT backend)
//! - google `https://cloudcode-pa.googleapis.com/` (Antigravity's Cloud Code endpoint)
//! - zai `https://api.z.ai/api/anthropic/v1/models` (omp z.ai models, models.yml baseUrl)
//!
//! Any HTTP status after a TLS handshake is reachable; a 403 "Request not allowed" or an
//! `unsupported_country` body is `blocked`; DNS/TCP/TLS failure or timeout is `down`.
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub const PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "https://api.anthropic.com/v1/models"),
    ("openai", "https://chatgpt.com/backend-api/"),
    ("google", "https://cloudcode-pa.googleapis.com/"),
    ("zai", "https://api.z.ai/api/anthropic/v1/models"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub provider: String,
    pub state: &'static str, // ok | blocked | down
    pub latency_ms: Option<u64>,
    pub checked_at: i64,
    pub error: Option<String>,
    /// when the provider left "ok" (0 while ok)
    pub since: i64,
}

#[derive(Default)]
pub struct Net {
    pub probes: HashMap<String, Probe>,
    pub since: i64,
    /// test-only: provider → URL override (unroutable address); notifications/retries off
    pub test_override: HashMap<String, String>,
}

pub static NET: LazyLock<Mutex<Net>> = LazyLock::new(Default::default);

/// curl exit code, HTTP status and the first bytes of the body → state, error.
pub fn classify(exit: i32, status: u16, body: &str) -> (&'static str, Option<String>) {
    if exit != 0 || status == 0 {
        let why = match exit {
            6 => "DNS resolution failed",
            7 => "connection failed",
            28 => "timed out",
            35 | 60 => "TLS handshake failed",
            _ => "no response",
        };
        return ("down", Some(format!("{why} (curl {exit})")));
    }
    let b = body.to_ascii_lowercase();
    if b.contains("unsupported_country") || (status == 403 && b.contains("request not allowed")) {
        return ("blocked", Some(format!("HTTP {status}: provider refuses this network")));
    }
    ("ok", None)
}

/// Proxy variables of a live agent process (agents connect with them, so the probe does too).
fn agent_proxy_env() -> Vec<(String, String)> {
    let procs = crate::session::scan_procs();
    let Some(p) = procs.values().flatten().next() else { return vec![] };
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy", "NO_PROXY", "no_proxy"]
        .iter()
        .filter_map(|k| crate::session::env_of(p.pid, k).map(|v| (k.to_string(), v)))
        .collect()
}

fn probe_one(provider: &str, url: &str, env: &[(String, String)]) -> Probe {
    let t0 = std::time::Instant::now();
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "--max-time", "5", "-r", "0-2047", "-o", "-", "-w", "\n%{http_code}", url]);
    // exactly the agents' proxy setting: none unless they have one
    for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy", "NO_PROXY", "no_proxy"] {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output();
    let ms = t0.elapsed().as_millis() as u64;
    let (exit, status, body) = match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
            (o.status.code().unwrap_or(-1), code.trim().parse::<u16>().unwrap_or(0), body.to_string())
        }
        Err(e) => (-1, 0, e.to_string()),
    };
    let (state, error) = classify(exit, status, &body);
    Probe { provider: provider.into(), state, latency_ms: (state != "down").then_some(ms), checked_at: now_ms(), error, since: 0 }
}

/// Transitions of one probe round: (provider, old state, new state).
/// (provider, old state, new state, when it left "ok" before this round; 0 if it was ok)
pub type Change = (String, &'static str, &'static str, i64);

/// Merge a round of probes; returns the state changes (first round: none).
pub fn apply(net: &mut Net, round: Vec<Probe>) -> Vec<Change> {
    let mut changes = vec![];
    for mut p in round {
        let prev = net.probes.get(&p.provider).cloned();
        p.since = match (&prev, p.state) {
            (_, "ok") => 0,
            (Some(o), _) if o.state != "ok" => o.since,
            _ => p.checked_at,
        };
        if let Some(o) = &prev {
            if o.state != p.state {
                changes.push((p.provider.clone(), o.state, p.state, o.since));
            }
        }
        net.probes.insert(p.provider.clone(), p);
    }
    let overall = overall(net);
    if overall == "ok" {
        net.since = 0;
    } else if net.since == 0 {
        net.since = now_ms();
    }
    changes
}

pub fn overall(net: &Net) -> &'static str {
    let n = net.probes.len();
    let ok = net.probes.values().filter(|p| p.state == "ok").count();
    if n == 0 || ok == n {
        "ok"
    } else if ok == 0 {
        "down"
    } else {
        "partial"
    }
}

/// One probe round (all providers, in parallel).
pub fn check() -> Vec<Change> {
    let env = agent_proxy_env();
    let overrides = NET.plock().test_override.clone();
    let round: Vec<Probe> = std::thread::scope(|s| {
        let probes = &crate::config::get().network.probes;
        let hs: Vec<_> = probes
            .iter()
            .map(|(p, url)| {
                let p = p.as_str();
                let url = overrides.get(p).cloned().unwrap_or_else(|| url.to_string());
                let env = &env;
                s.spawn(move || probe_one(p, &url, env))
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    apply(&mut NET.plock(), round)
}

pub fn view() -> Value {
    let n = NET.plock();
    let mut ps: Vec<&Probe> = n.probes.values().collect();
    ps.sort_by_key(|p| PROVIDERS.iter().position(|x| x.0 == p.provider));
    json!({
        "state": overall(&n),
        "since": (n.since > 0).then_some(n.since),
        "providers": ps.iter().map(|p| json!({"provider": p.provider, "state": p.state, "latencyMs": p.latency_ms, "checkedAt": p.checked_at, "error": p.error, "since": (p.since > 0).then_some(p.since)})).collect::<Vec<_>>(),
        "test": !n.test_override.is_empty(),
    })
}

/// Provider an agent talks to (model prefix first, then harness).
pub fn provider_of(agent_type: &str, model: Option<&str>) -> Option<&'static str> {
    let m = model.unwrap_or("").to_ascii_lowercase();
    if m.starts_with("zai/") || m.contains("glm") {
        return Some("zai");
    }
    if m.starts_with("openai") || m.contains("gpt") || m.contains("codex") {
        return Some("openai");
    }
    if m.starts_with("google") || m.contains("gemini") {
        return Some("google");
    }
    if m.starts_with("anthropic/") || m.contains("claude") {
        return Some("anthropic");
    }
    match agent_type {
        "claude" => Some("anthropic"),
        "codex" => Some("openai"),
        "antigravity" => Some("google"),
        "omp" => Some("anthropic"),
        _ => None,
    }
}

/// The agent's provider is not reachable now (adash-initiated sends wait).
pub fn held(agent_type: &str, model: Option<&str>) -> bool {
    let Some(p) = provider_of(agent_type, model) else { return false };
    NET.plock().probes.get(p).is_some_and(|x| x.state != "ok")
}

pub fn agent_held(a: &crate::collect::Agent) -> bool {
    held(&a.agent_type, a.model.as_deref())
}

/// A provider error that a working network fixes (not auth, quota or a bad request).
pub fn network_class(status: Option<i64>, message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    if matches!(status, Some(401 | 429 | 400 | 404 | 413 | 422))
        || m.contains("rate limit")
        || m.contains("quota")
        || m.contains("usage limit")
        || m.contains("invalid")
        || m.contains("refresh token")
    {
        return false;
    }
    matches!(status, Some(500..=599))
        || (status == Some(403) && (m.contains("request not allowed") || m.contains("unsupported_country")))
        || [
            "connection",
            "econnreset",
            "socket",
            "timed out",
            "timeout",
            "dns",
            "enotfound",
            "getaddrinfo",
            "tls",
            "certificate",
            "network",
            "stream stalled",
            "fetch failed",
            "unsupported_country",
        ]
        .iter()
        .any(|k| m.contains(k))
}

/// Next probe interval: 10 s while anything is down/blocked, else 30 s.
pub fn interval_s() -> u64 {
    if overall(&NET.plock()) == "ok" {
        30
    } else {
        10
    }
}

pub fn display(p: &str) -> &'static str {
    match p {
        "anthropic" => "Anthropic",
        "openai" => "OpenAI",
        "google" => "Google",
        "zai" => "z.ai",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(p: &str, state: &'static str, at: i64) -> Probe {
        Probe { provider: p.into(), state, latency_ms: None, checked_at: at, error: None, since: 0 }
    }

    #[test]
    fn classify_reachable_blocked_down() {
        assert_eq!(classify(0, 401, r#"{"type":"error"}"#).0, "ok", "any status from the API host is reachable");
        assert_eq!(classify(0, 403, r#"{"error":{"type":"forbidden","message":"Request not allowed"}}"#).0, "blocked");
        assert_eq!(classify(0, 403, r#"{"error":{"code":"unsupported_country_region_territory"}}"#).0, "blocked");
        assert_eq!(classify(6, 0, "").0, "down");
        assert_eq!(classify(28, 0, "").0, "down");
    }

    #[test]
    fn transitions_and_overall_state() {
        let mut n = Net::default();
        assert!(apply(&mut n, vec![probe("anthropic", "ok", 1), probe("openai", "ok", 1)]).is_empty());
        let ch = apply(&mut n, vec![probe("anthropic", "ok", 2), probe("openai", "blocked", 2)]);
        assert_eq!(ch, vec![("openai".to_string(), "ok", "blocked", 0)]);
        assert_eq!(overall(&n), "partial");
        assert_eq!(n.probes["openai"].since, 2);
        apply(&mut n, vec![probe("anthropic", "down", 3), probe("openai", "down", 3)]);
        assert_eq!((overall(&n), n.probes["openai"].since), ("down", 2), "since stays at the first failure");
        let ch = apply(&mut n, vec![probe("anthropic", "ok", 4), probe("openai", "ok", 4)]);
        assert_eq!(ch.len(), 2);
        assert!(ch.contains(&("openai".to_string(), "down", "ok", 2)), "recovery carries when it went down");
        assert_eq!((overall(&n), n.since), ("ok", 0));
    }

    #[test]
    fn only_network_errors_are_retried() {
        assert!(network_class(None, "Connection error."));
        assert!(network_class(None, "ECONNRESET: The socket connection was closed unexpectedly"));
        assert!(network_class(Some(403), "Request not allowed"));
        assert!(network_class(Some(503), "Service unavailable"));
        assert!(!network_class(Some(429), "Rate limit reached"));
        assert!(!network_class(Some(401), "Your access token could not be refreshed"));
        assert!(!network_class(Some(400), "invalid_request_error"));
        assert_eq!(provider_of("omp", Some("openai-codex/gpt-5.5")), Some("openai"));
        assert_eq!(provider_of("omp", Some("zai/glm-5.1")), Some("zai"));
        assert_eq!(provider_of("claude", None), Some("anthropic"));
    }
}
