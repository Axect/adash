//! Agents that sat idle for a long time are put to sleep: closed gracefully (the session stays),
//! remembered in `sleep.json`, kept on the board as "asleep", and reopened on the same session
//! when something has to reach them (a message, an answer, "다시 열기", a thread or magi run).
//! A tmux server restart (reboot) does not bring them back: they have no terminal to restore.
use crate::collect::{Agent, Snapshot};
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Minutes between sleep rounds.
const ROUND_MIN: i64 = 30;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Store {
    /// identity → {name, project, agentType, model, sessionPath, idleSince, sleptAt}
    pub asleep: BTreeMap<String, Value>,
    /// identities that never sleep
    pub pinned: BTreeSet<String>,
}

fn path() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-sleep-{}.json", std::process::id()));
    }
    crate::util::state_dir().join("sleep.json")
}

static LOCK: Mutex<()> = Mutex::new(());
/// identities being woken now (the board says "깨우는 중…")
pub static WAKING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
static LAST_ROUND: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

impl Store {
    pub fn load() -> Store {
        std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save(&self) {
        let _ = crate::util::atomic_write(&path(), serde_json::to_string_pretty(self).unwrap().as_bytes());
    }
    /// The sleeping agent a name or identity refers to.
    pub fn find(&self, key: &str) -> Option<(String, Value)> {
        if let Some(v) = self.asleep.get(key) {
            return Some((key.to_string(), v.clone()));
        }
        self.asleep.iter().find(|(_, v)| v["name"] == key).map(|(k, v)| (k.clone(), v.clone()))
    }
}

fn log(action: &str, name: &str, detail: Value) {
    if let Ok(db) = crate::analytics::open() {
        db.log_action(action, name, &detail);
    }
}

/// Pin (never sleep) or unpin an agent by identity.
pub fn pin(identity: &str, on: bool) {
    let _g = LOCK.plock();
    let mut s = Store::load();
    if on {
        s.pinned.insert(identity.to_string());
    } else {
        s.pinned.remove(identity);
    }
    s.save();
}

/// Terminal handles a tmux client is looking at now.
fn watched() -> HashSet<String> {
    let panes: HashSet<String> = crate::term::tmux(&["list-clients", "-F", "#{pane_id}"]).unwrap_or_default().lines().map(|l| l.trim().to_string()).collect();
    crate::term::panes().unwrap_or_default().into_iter().filter(|p| panes.contains(&p.pane_id)).map(|p| p.handle).collect()
}

/// Why `a` may not sleep now (None = it may).
fn blocker(a: &Agent, now: i64, after_ms: i64, st: &Store, busy: &HashSet<String>, watched: &HashSet<String>) -> Option<&'static str> {
    if !matches!(a.state.as_str(), "done" | "idle") {
        return Some("state");
    }
    if now - a.state_since < after_ms {
        return Some("recent");
    }
    if a.handle.is_none() || a.session_path.as_ref().is_none_or(|p| !p.exists()) {
        return Some("not_reopenable");
    }
    if st.pinned.contains(&a.identity) {
        return Some("pinned");
    }
    if a.draft.as_ref().is_some_and(|d| !d.is_null()) {
        return Some("draft");
    }
    if a.unread {
        return Some("unread");
    }
    if a.snoozed_until.is_some_and(|t| t > now) {
        return Some("snoozed");
    }
    if busy.contains(&a.name) {
        return Some("thread");
    }
    if crate::outdated::WHEN_IDLE.plock().contains(&a.name) {
        return Some("restart_pending");
    }
    if a.handle.as_ref().is_some_and(|h| watched.contains(h)) {
        return Some("watched");
    }
    None
}

/// Names an open adash thread or magi run still has to reach. Detected exchanges (phase
/// "external") stay "delivered" forever and threads older than the sleep window are history:
/// neither keeps an agent awake.
fn busy_names() -> HashSet<String> {
    let mut out = HashSet::new();
    let since = now_ms() - (crate::config::get().sleep.after_hours.max(1) as i64) * 3_600_000;
    for t in crate::threads::load_all().into_iter().filter(|t| t.phase != "external" && t.created_at >= since) {
        if (!matches!(t.status.as_str(), "answered" | "done" | "failed" | "cancelled") || t.phase == "relay") && t.phase != "done" {
            out.insert(t.to.clone());
            out.insert(t.from.clone());
        }
    }
    for r in crate::magi::RunState::load_all().into_iter().filter(|r| matches!(r.status.as_str(), "queued" | "held" | "delivered")) {
        out.insert(r.source.clone());
        out.extend(r.targets.iter().cloned());
    }
    out
}

/// Every live agent with its verdict: `{name, harness, idleHours, sleep: bool, reason?}`.
pub fn plan(snap: &Snapshot) -> Vec<Value> {
    let cfg = crate::config::get().sleep.clone();
    let after = (cfg.after_hours.max(1) as i64) * 3_600_000;
    let now = now_ms();
    let st = Store::load();
    let (busy, w) = (busy_names(), watched());
    snap.agents
        .iter()
        .filter(|a| a.handle.is_some())
        .map(|a| {
            let why = blocker(a, now, after, &st, &busy, &w);
            json!({"name": a.name, "identity": a.identity, "harness": a.agent_type, "project": a.project, "state": a.state,
                   "idleHours": ((now - a.state_since) as f64 / 3_600_000.0 * 10.0).round() / 10.0, "sleep": why.is_none(), "reason": why})
        })
        .collect()
}

/// One round (every 30 min): the due agents go to sleep, at most `[sleep] perRound` per harness
/// (0 = no limit). `force` skips the interval.
pub fn round(sh: &crate::server::Shared, force: bool) -> Vec<Value> {
    let cfg = crate::config::get().sleep.clone();
    if !cfg.enabled {
        return vec![];
    }
    let now = now_ms();
    let last = LAST_ROUND.load(std::sync::atomic::Ordering::SeqCst);
    if !force && now - last < ROUND_MIN * 60_000 {
        return vec![];
    }
    LAST_ROUND.store(now, std::sync::atomic::Ordering::SeqCst);
    let snap = sh.snap.plock().clone();
    let mut per: BTreeMap<String, usize> = BTreeMap::new();
    let mut ready: Vec<Agent> = plan(&snap)
        .into_iter()
        .filter(|v| v["sleep"] == true)
        .filter_map(|v| snap.agents.iter().find(|a| a.identity == v["identity"].as_str().unwrap_or("")).cloned())
        .collect();
    // longest idle first
    ready.sort_by_key(|a| a.state_since);
    let picks: Vec<Agent> = ready
        .into_iter()
        .filter(|a| {
            let n = per.entry(a.agent_type.clone()).or_default();
            *n += 1;
            cfg.per_round == 0 || *n <= cfg.per_round as usize
        })
        .collect();
    let mut out = vec![];
    for a in picks {
        out.push(put_to_sleep(sh, &a));
    }
    out
}

/// Put one agent to sleep now (menu "잠재우기"): every rule but the idle time applies.
pub fn sleep_now(sh: &crate::server::Shared, key: &str) -> Result<Value, String> {
    let snap = sh.snap.plock().clone();
    let a = snap.agents.iter().find(|a| a.name == key || a.identity == key).ok_or_else(|| format!("unknown agent {key}"))?;
    let mut st = Store::load();
    st.pinned.remove(&a.identity);
    if let Some(why) = blocker(a, now_ms(), 0, &st, &busy_names(), &watched()) {
        return Err(format!("cannot sleep now: {why}"));
    }
    Ok(put_to_sleep(sh, a))
}

/// Close one agent gracefully and remember it as asleep; the screen and state are read again
/// first (an agent that started work or got a question since the plan stays awake).
fn put_to_sleep(sh: &crate::server::Shared, a: &Agent) -> Value {
    let snap = sh.snap.plock().clone();
    let Some(cur) = snap.agents.iter().find(|x| x.identity == a.identity) else { return json!({"name": a.name, "ok": false, "reason": "gone"}) };
    if cur.state != a.state || cur.state_since != a.state_since {
        return json!({"name": a.name, "ok": false, "reason": "changed"});
    }
    let h = cur.handle.clone().unwrap_or_default();
    let ready = crate::term::screen(&h).ok().and_then(|t| crate::threads::composer_empty(&cur.agent_type, "screen", &t));
    if ready != Some(true) {
        return json!({"name": a.name, "ok": false, "reason": "composer"});
    }
    match crate::lifecycle::close(&snap, cur, &json!({"mode": "graceful"})) {
        Ok(v) => {
            let _g = LOCK.plock();
            let mut st = Store::load();
            st.asleep.insert(
                cur.identity.clone(),
                json!({"name": cur.name, "project": cur.project, "agentType": cur.agent_type, "model": cur.model, "sessionPath": cur.session_path,
                       "idleSince": cur.state_since, "sleptAt": now_ms()}),
            );
            st.save();
            log("sleep", &cur.name, json!({"identity": cur.identity, "idleSince": cur.state_since, "close": v}));
            json!({"name": cur.name, "ok": true})
        }
        Err((code, v)) => json!({"name": cur.name, "ok": false, "code": code, "reason": v}),
    }
}

/// Reopen a sleeping agent (name or identity) on its session and wait until its input box is
/// empty and steady (≤ 90 s). Returns the new terminal handle; None when `key` is not asleep.
pub fn wake(sh: &crate::server::Shared, key: &str, why: &str) -> Option<Result<String, String>> {
    let (id, e) = Store::load().find(key)?;
    if !WAKING.plock().insert(id.clone()) {
        // another request wakes it: wait for that one
        for _ in 0..180 {
            std::thread::sleep(Duration::from_millis(500));
            if !WAKING.plock().contains(&id) {
                break;
            }
        }
        return Some(sh.snap.plock().agents.iter().find(|a| a.identity == id).and_then(|a| a.handle.clone()).ok_or_else(|| "wake failed".into()));
    }
    let r = (|| -> Result<String, String> {
        let snap = sh.snap.plock().clone();
        let v = crate::lifecycle::reopen(&snap, &id).map_err(|(_, v)| v["error"].as_str().or(v["reason"].as_str()).unwrap_or("reopen failed").to_string())?;
        let h = v["handle"].as_str().ok_or("no handle")?.to_string();
        // the folder was trusted before: accept the trust screen if the harness asks again
        crate::launch::AUTO_TRUST.plock().insert(h.clone());
        let kind = e["agentType"].as_str().unwrap_or("omp").to_string();
        let mut ok = 0;
        for _ in 0..90 {
            std::thread::sleep(Duration::from_secs(1));
            if kind == "antigravity" {
                crate::server::dismiss_agy_notice(&h);
            }
            let empty = crate::term::screen(&h).ok().and_then(|t| crate::threads::composer_empty(&kind, "screen", &t));
            ok = if empty == Some(true) { ok + 1 } else { 0 };
            if ok >= 3 {
                break;
            }
        }
        if ok < 3 {
            return Err("the agent did not reach its input box within 90 s".into());
        }
        Ok(h)
    })();
    if r.is_ok() {
        let _g = LOCK.plock();
        let mut st = Store::load();
        st.asleep.remove(&id);
        st.save();
    }
    log("wake", e["name"].as_str().unwrap_or(key), json!({"identity": id, "why": why, "ok": r.is_ok(), "error": r.as_ref().err()}));
    WAKING.plock().remove(&id);
    Some(r)
}

/// The board's view: `[{name, identity, project, agentType, model, idleSince, sleptAt, waking, pinned}]`
/// plus the pinned identities.
pub fn view() -> Value {
    let st = Store::load();
    let waking = WAKING.plock().clone();
    let list: Vec<Value> = st
        .asleep
        .iter()
        .map(|(id, v)| {
            let mut v = v.clone();
            v["identity"] = json!(id);
            v["waking"] = json!(waking.contains(id));
            v
        })
        .collect();
    json!({"asleep": list, "pinned": st.pinned})
}

/// Drop entries whose agent is live again (reopened by hand, or by a restore).
pub fn prune(snap: &Snapshot) {
    let _g = LOCK.plock();
    let mut st = Store::load();
    let live: HashSet<&str> = snap.agents.iter().map(|a| a.identity.as_str()).collect();
    let before = st.asleep.len();
    st.asleep.retain(|id, _| !live.contains(id.as_str()) || WAKING.plock().contains(id));
    if st.asleep.len() != before {
        st.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_long_idle_quiet_agents_sleep() {
        let now = now_ms();
        let file = std::env::temp_dir().join(format!("adash-sleep-test-{}.jsonl", std::process::id()));
        std::fs::write(&file, "").unwrap();
        let a = Agent {
            name: "A".into(),
            identity: "id-a".into(),
            state: "done".into(),
            state_since: now - 25 * 3_600_000,
            handle: Some("t_1".into()),
            session_path: Some(file.clone()),
            ..Default::default()
        };
        let (st, busy, w) = (Store::default(), HashSet::new(), HashSet::new());
        let day = 24 * 3_600_000;
        assert_eq!(blocker(&a, now, day, &st, &busy, &w), None);
        assert_eq!(blocker(&Agent { state_since: now - 3_600_000, ..a.clone() }, now, day, &st, &busy, &w), Some("recent"));
        assert_eq!(blocker(&Agent { state: "asking".into(), ..a.clone() }, now, day, &st, &busy, &w), Some("state"));
        assert_eq!(blocker(&Agent { unread: true, ..a.clone() }, now, day, &st, &busy, &w), Some("unread"));
        assert_eq!(blocker(&Agent { draft: Some(json!({"text": "x"})), ..a.clone() }, now, day, &st, &busy, &w), Some("draft"));
        let pinned = Store { pinned: ["id-a".to_string()].into(), ..Default::default() };
        assert_eq!(blocker(&a, now, day, &pinned, &busy, &w), Some("pinned"));
        assert_eq!(blocker(&a, now, day, &st, &["A".to_string()].into(), &w), Some("thread"));
        assert_eq!(blocker(&a, now, day, &st, &busy, &["t_1".to_string()].into()), Some("watched"));
        let _ = std::fs::remove_file(&file);
    }
}
