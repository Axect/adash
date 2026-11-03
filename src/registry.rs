//! Per-agent persistent state keyed by stable identity (`~/.local/state/adash/registry.json`):
//! user overrides, self-declared names, snoozes and name requests.
use crate::util::{atomic_write, state_dir, Res};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SelfName {
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(rename = "setAt", default)]
    pub set_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NameRequest {
    pub name_requested_at: i64,
    /// line delivered (None while queued behind the delivery gate)
    #[serde(default)]
    pub sent_at: Option<i64>,
    /// waitingSince / needsHuman before the naming turn, restored after it
    #[serde(default)]
    pub prev_since: i64,
    #[serde(default)]
    pub prev_needs_human: bool,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub self_names: BTreeMap<String, SelfName>,
    #[serde(default)]
    pub snoozes: BTreeMap<String, i64>,
    #[serde(default)]
    pub name_requests: BTreeMap<String, NameRequest>,
    /// names chosen at launch, keyed by terminal handle until the agent shows up
    #[serde(default)]
    pub pending_names: BTreeMap<String, SelfName>,
    /// "확인함": identity → ackedAt (ms)
    #[serde(default)]
    pub acked: BTreeMap<String, i64>,
}

fn path() -> std::path::PathBuf {
    state_dir().join("registry.json")
}

pub const NAME_REQUEST_COOLDOWN_MS: i64 = 24 * 3_600_000;
/// A naming turn finishing later than this after delivery is treated as a normal turn.
pub const NAME_TURN_WINDOW_MS: i64 = 10 * 60_000;

/// Name shape rule for self-declared names: 2–20 chars, Korean/English letters, digits, space, - _ ·
pub fn validate_name(n: &str) -> Res<()> {
    let len = n.chars().count();
    if !(2..=20).contains(&len) {
        return Err(format!("이름은 2~20자여야 합니다 (현재 {len}자)"));
    }
    let ok = n.chars().all(|c| {
        c.is_ascii_alphanumeric() || ('\u{AC00}'..='\u{D7A3}').contains(&c) || ('\u{3131}'..='\u{318E}').contains(&c) || matches!(c, ' ' | '-' | '_' | '·')
    });
    if !ok || n.trim() != n {
        return Err("이름에는 한글·영문·숫자·공백·- _ · 만 쓸 수 있습니다 (앞뒤 공백 불가)".into());
    }
    Ok(())
}

impl Registry {
    pub fn load() -> Registry {
        std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Res<()> {
        crate::util::guard_config(&path(), |t| serde_json::from_str::<Registry>(t).is_ok())?;
        atomic_write(&path(), serde_json::to_string_pretty(self).unwrap().as_bytes())
    }

    /// An agent first seen by paneKey later resolves to its session file: carry its state over.
    pub fn migrate(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        fn mv<V>(m: &mut BTreeMap<String, V>, from: &str, to: &str) {
            if let Some(v) = m.remove(from) {
                m.entry(to.to_string()).or_insert(v);
            }
        }
        mv(&mut self.overrides, from, to);
        mv(&mut self.self_names, from, to);
        mv(&mut self.snoozes, from, to);
        mv(&mut self.name_requests, from, to);
        mv(&mut self.acked, from, to);
    }

    /// Resolve display names. `wanted`: (identity, codename, fallback base, title).
    /// Priority: override > self > codename unique among live agents > fallback. No two live
    /// agents ever show the same name, whatever the source: a chosen name (override/self)
    /// claims it first; a title codename equal to a chosen name or shared by several agents is
    /// marked ambiguous and shown as `<CODE> · <first title words>` (≤24 chars); any remaining
    /// clash gets " (2)", " (3)".
    pub fn names(&self, wanted: &[(String, Option<String>, String, String)]) -> HashMap<String, (String, &'static str, bool)> {
        let chosen = |id: &str| self.overrides.get(id).map(|n| (n.clone(), "override")).or_else(|| self.self_names.get(id).map(|s| (s.name.clone(), "self")));
        let mut code_count: HashMap<&str, usize> = HashMap::new();
        let mut claimed: HashSet<String> = HashSet::new();
        for (id, code, _, _) in wanted {
            match chosen(id) {
                Some((n, _)) => {
                    claimed.insert(n);
                }
                None => {
                    if let Some(c) = code {
                        *code_count.entry(c.as_str()).or_default() += 1;
                    }
                }
            }
        }
        let mut taken: HashSet<String> = HashSet::new();
        let mut out = HashMap::new();
        // chosen names first so they keep their name when a codename collides
        let mut order: Vec<&(String, Option<String>, String, String)> = wanted.iter().collect();
        order.sort_by_key(|w| chosen(&w.0).is_none());
        for (id, code, base, title) in order {
            let mut v = if let Some((n, src)) = chosen(id) {
                if taken.contains(&n) {
                    (with_words(&n, title), src, true)
                } else {
                    (n, src, false)
                }
            } else if let Some(c) = code.as_deref().filter(|c| code_count.get(c) == Some(&1) && !claimed.contains(*c)) {
                (c.to_string(), "title", false)
            } else if let Some(c) = code {
                (with_words(c, title), "title", true)
            } else {
                (with_words(base, title), "fallback", true)
            };
            if taken.contains(&v.0) {
                let stem = v.0.clone();
                let mut k = 2;
                while taken.contains(&format!("{stem} ({k})")) {
                    k += 1;
                }
                v = (format!("{stem} ({k})"), v.1, true);
            }
            taken.insert(v.0.clone());
            out.insert(id.clone(), v);
        }
        out
    }

    /// Helper sessions (peer review / magi): "<requester>-<role>" unless the user named it.
    pub fn helper_name(&mut self, identity: &str, requester: &str, role: &str) -> Option<String> {
        if self.overrides.contains_key(identity) || self.self_names.contains_key(identity) || requester.is_empty() || role.is_empty() {
            return None;
        }
        let n = format!("{requester}-{role}");
        self.overrides.insert(identity.to_string(), n.clone());
        Some(n)
    }

    pub fn prune(&mut self, now: i64) {
        self.snoozes.retain(|_, until| *until > now);
        self.name_requests.retain(|_, r| now - r.name_requested_at < 7 * 86_400_000);
    }
}

/// "DASH" + "Prioritize and Organize Job Applications" → "DASH · Prioritize and Or…" (≤24 chars, word cut)
pub fn with_words(head: &str, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return head.to_string();
    }
    let mut out = format!("{head} ·");
    for w in title.split_whitespace() {
        if out.chars().count() + 1 + w.chars().count() > 24 {
            break;
        }
        out.push(' ');
        out.push_str(w);
    }
    if out.ends_with('·') {
        // first word alone is too long: hard cut
        let room = 24usize.saturating_sub(out.chars().count() + 1);
        out.push(' ');
        out.extend(title.chars().take(room));
    }
    out
}

#[cfg(test)]
mod name_tests {

    #[test]
    fn live_names_never_collide_across_sources() {
        let mut r = super::Registry::default();
        r.overrides.insert("main".into(), "PLANNER".into());
        let w = |id: &str, code: Option<&str>, title: &str| (id.to_string(), code.map(String::from), "galaxy-survey-omp".to_string(), title.to_string());
        let n = r.names(&[
            w("astra", Some("PLANNER"), "Critique transition claims"),
            w("main", Some("PLANNER"), "Revise statement"),
            w("argon", None, "Argon review"),
        ]);
        assert_eq!(n["main"], ("PLANNER".to_string(), "override", false));
        assert_eq!(n["astra"].0, "PLANNER · Critique");
        assert!(n["astra"].2, "a codename taken by a chosen name is ambiguous");
        // two overrides with the same name: the second is set apart
        r.overrides.insert("astra".into(), "PLANNER".into());
        let n = r.names(&[w("main", None, "a"), w("astra", None, "b")]);
        assert_ne!(n["main"].0, n["astra"].0);
        // helper sessions get <requester>-<role> once, never over a user's name
        assert_eq!(r.helper_name("argon", "PLANNER", "Argon").as_deref(), Some("PLANNER-Argon"));
        assert_eq!(r.helper_name("argon", "BRIEF", "Argon"), None);
    }
}
