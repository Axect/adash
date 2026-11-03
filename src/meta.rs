//! Manual metadata in `~/.config/adash/meta.yaml`.
use crate::util::{atomic_write, meta_path, Res};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn d3() -> u8 {
    3
}
fn thresholds() -> [f64; 2] {
    [5.0, 60.0]
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMeta {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub goal: String,
    #[serde(default = "d3")]
    pub impact: u8,
    #[serde(default = "d3")]
    pub leverage: u8,
    #[serde(default = "d3")]
    pub cost: u8,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub note: String,
    /// hidden from the board, counts and notifications; `until` null = until unhidden
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Hidden>,
    /// closed and hidden for good ("보관"); restore = unhide
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// sessions that were live when the project was archived (for "reopen all")
    #[serde(default, rename = "archivedSessions", skip_serializing_if = "Vec::is_empty")]
    pub archived_sessions: Vec<ArchivedSession>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Hidden {
    pub until: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArchivedSession {
    pub name: String,
    pub identity: String,
}

impl Default for ProjectMeta {
    fn default() -> Self {
        ProjectMeta {
            paths: vec![],
            goal: String::new(),
            impact: 3,
            leverage: 3,
            cost: 3,
            deadline: None,
            note: String::new(),
            hidden: None,
            archived: false,
            archived_sessions: vec![],
        }
    }
}

impl ProjectMeta {
    /// Hidden now (an `until` in the past has expired).
    pub fn is_hidden(&self, now: i64) -> bool {
        self.hidden.as_ref().is_some_and(|h| h.until.is_none_or(|u| u > now))
    }
}

impl ProjectMeta {
    pub fn score(&self) -> f64 {
        let c = self.cost.clamp(1, 5) as f64;
        (self.impact.clamp(1, 5) as f64 + 2.0 * self.leverage.clamp(1, 5) as f64) / c
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HumanTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub done: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AgentMeta {
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub priority_bonus: f64,
}

/// Prompt-cache TTL per provider/model (seconds); base time = last agent activity.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CacheTtl {
    #[serde(default = "ttl_default")]
    pub default: u64,
    #[serde(default = "ttl_providers")]
    pub by_provider: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_model: BTreeMap<String, u64>,
    /// "<harness>/<provider>" (e.g. "omp/anthropic"): the harness decides retention and warming
    #[serde(default = "ttl_harness")]
    pub by_harness: BTreeMap<String, u64>,
}

/// Defaults from sources and October 2026 measurements (first request of each human turn, hit
/// = cache reads dominate; `adash` analytics, idle gap = previous turn end → message):
/// - omp/anthropic 60 min: omp's `cacheRetention: auto` picks Anthropic's 1 h entries
///   ("long", warmed only during active runs; idle warming of 5 m entries stops at a 30-minute
///   idle limit). Measured hit 82–93 % up to 60 min, 9 % at 60–90 min, ~0 after.
/// - claude/anthropic 60 min: Claude Code writes `ephemeral_1h` entries.
/// - omp/openai, codex/openai 40 min: OpenAI keeps prompt caches for minutes to an hour
///   without a fixed TTL; measured 90–95 % under 10 min, 67–79 % at 10–40 min, 39 % at 40–60.
/// - omp/zai 5 min: cache share falls to ~50 % at 5–10 min.
/// - antigravity/google 5 min: no measurements; Gemini implicit caching has no stated TTL.
fn ttl_harness() -> BTreeMap<String, u64> {
    [("omp/anthropic", 3600), ("claude/anthropic", 3600), ("omp/openai", 2400), ("codex/openai", 2400), ("omp/zai", 300), ("antigravity/google", 300)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// One-line reason for a TTL shown in the UI tooltip.
pub fn ttl_basis(key: &str) -> &'static str {
    match key {
        "omp/anthropic" => "omp+Anthropic: 1시간 캐시(cacheRetention auto), 실측 약 60분",
        "claude/anthropic" => "Claude Code: 1시간 캐시(ephemeral_1h)",
        "omp/openai" | "codex/openai" => "OpenAI: 고정 TTL 없음, 실측 10–40분 적중 약 70%, 40분 넘으면 대부분 식음",
        "omp/zai" => "z.ai: 실측 5분 뒤 절반 이하",
        "antigravity/google" => "Gemini: 문서상 TTL 없음, 보수적 5분",
        _ => "기본 5분",
    }
}

fn ttl_default() -> u64 {
    300
}
fn ttl_providers() -> BTreeMap<String, u64> {
    [("anthropic", 300), ("openai", 300), ("google", 300)].into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

impl Default for CacheTtl {
    fn default() -> Self {
        CacheTtl { default: ttl_default(), by_provider: ttl_providers(), by_model: BTreeMap::new(), by_harness: ttl_harness() }
    }
}

impl CacheTtl {
    /// (seconds, key it came from): model override > harness/provider > provider > default.
    pub fn ttl(&self, agent_type: &str, model: Option<&str>) -> (u64, String) {
        if let Some(m) = model {
            let base = m.split(':').next().unwrap_or(m);
            let short = base.rsplit('/').next().unwrap_or(base);
            if let Some(t) = self.by_model.get(base).or_else(|| self.by_model.get(short)) {
                return (*t, format!("model {short}"));
            }
        }
        let provider = crate::network::provider_of(agent_type, model).unwrap_or("");
        let key = format!("{agent_type}/{provider}");
        if let Some(t) = self.by_harness.get(&key) {
            return (*t, key);
        }
        (self.by_provider.get(provider).copied().unwrap_or(self.default), key)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    #[serde(default = "thresholds")]
    pub cache_thresholds_min: [f64; 2],
    #[serde(default)]
    pub cache_ttl: CacheTtl,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectMeta>,
    #[serde(default)]
    pub human_tasks: Vec<HumanTask>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentMeta>,
    /// tailnet logins allowed through `tailscale serve` (empty: the tailnet owner)
    #[serde(default, rename = "mobileAllowedLogins")]
    pub mobile_allowed: Vec<String>,
    /// retry agents stopped by a network error once their provider is reachable again (default on)
    #[serde(default, rename = "autoRetryOnNetworkRecovery", skip_serializing_if = "Option::is_none")]
    pub auto_retry_on_network_recovery: Option<bool>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            cache_thresholds_min: thresholds(),
            cache_ttl: CacheTtl::default(),
            projects: BTreeMap::new(),
            human_tasks: vec![],
            agents: BTreeMap::new(),
            mobile_allowed: vec![],
            auto_retry_on_network_recovery: None,
        }
    }
}

impl Meta {
    pub fn load() -> Meta {
        match std::fs::read_to_string(meta_path()) {
            Ok(s) => serde_yaml::from_str(&s).unwrap_or_else(|e| {
                eprintln!("adash: meta.yaml parse error, using defaults: {e}");
                Meta::default()
            }),
            Err(_) => Meta::default(),
        }
    }

    pub fn save(&self) -> Res<()> {
        crate::util::guard_config(&meta_path(), |t| serde_yaml::from_str::<Meta>(t).is_ok())?;
        let y = serde_yaml::to_string(self).map_err(|e| e.to_string())?;
        atomic_write(&meta_path(), y.as_bytes())
    }

    /// Project name override by configured path.
    /// Projects hidden right now.
    pub fn hidden_projects(&self, now: i64) -> std::collections::HashSet<String> {
        self.projects.iter().filter(|(_, p)| p.is_hidden(now)).map(|(n, _)| n.clone()).collect()
    }

    pub fn project_for_path(&self, path: &str) -> Option<String> {
        self.projects.iter().find(|(_, p)| p.paths.iter().any(|x| x.trim_end_matches('/') == path.trim_end_matches('/'))).map(|(n, _)| n.clone())
    }

    pub fn next_task_id(&self) -> String {
        let mut n = 1;
        while self.human_tasks.iter().any(|t| t.id == format!("h{n}")) {
            n += 1;
        }
        format!("h{n}")
    }
}

#[cfg(test)]
mod ttl_tests {
    #[test]
    fn ttl_follows_harness_and_provider() {
        let t = super::CacheTtl::default();
        assert_eq!(t.ttl("omp", Some("anthropic/claude-opus-5-5:high")), (3600, "omp/anthropic".to_string()));
        assert_eq!(t.ttl("claude", Some("claude-opus-5-5")).0, 3600);
        assert_eq!(t.ttl("omp", Some("openai-codex/gpt-6-sol")).0, 2400);
        assert_eq!(t.ttl("codex", None).0, 2400);
        assert_eq!(t.ttl("omp", Some("zai/glm-5.1")).0, 300);
        assert_eq!(t.ttl("antigravity", Some("gemini-4-argon")).0, 300);
        let mut o = t.clone();
        o.by_model.insert("claude-opus-5-5".into(), 120);
        assert_eq!(o.ttl("omp", Some("anthropic/claude-opus-5-5")).0, 120, "a model override wins");
        assert!(super::ttl_basis("omp/anthropic").contains("60분"));
    }
}

#[cfg(test)]
mod hide_tests {
    use super::*;

    #[test]
    fn hidden_and_archived_round_trip_and_expire() {
        let mut m = Meta::default();
        let p = m.projects.entry("demo".into()).or_default();
        p.hidden = Some(Hidden { until: Some(1_000) });
        p.archived = true;
        p.archived_sessions = vec![ArchivedSession { name: "A".into(), identity: "/s/a.jsonl".into() }];
        let back: Meta = serde_yaml::from_str(&serde_yaml::to_string(&m).unwrap()).unwrap();
        let p = &back.projects["demo"];
        assert_eq!((p.archived, p.archived_sessions.len()), (true, 1));
        assert!(p.is_hidden(999) && !p.is_hidden(1_001), "a timed hide expires");
        assert!(ProjectMeta { hidden: Some(Hidden { until: None }), ..Default::default() }.is_hidden(i64::MAX));
        assert_eq!(back.hidden_projects(500).len(), 1);
        // a project without the new keys still parses (older meta.yaml)
        let old: ProjectMeta = serde_yaml::from_str("paths: [/w]\ngoal: g\n").unwrap();
        assert!(!old.is_hidden(0) && !old.archived);
    }
}
