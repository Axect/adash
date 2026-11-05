//! Desktop notifications (`platform::notify`: notify-send, terminal-notifier or osascript; with
//! notify-send the "열기" action focuses the terminal).
use crate::collect::{Agent, Snapshot};
use crate::util::truncate;
use std::collections::{HashMap, HashSet};

/// Sent notifications waiting to be recorded in the analytics store: (ts, title, body, handle).
pub static PENDING: std::sync::Mutex<Vec<(i64, String, String, String)>> = std::sync::Mutex::new(Vec::new());

pub fn send(summary: &str, body: &str, focus_handle: Option<String>) {
    send_for(summary, body, focus_handle, None);
}

/// `send` about one agent: clicking the notification opens its chat on the dashboard.
pub fn send_for(summary: &str, body: &str, focus_handle: Option<String>, agent: Option<&str>) {
    // phone push: every event class except plain transitions (those go through `send_transition`)
    let ev = crate::push::event_of(summary);
    if ev != "waiting_after_human" {
        push(ev, summary, body, None);
    }
    desktop(summary, body, focus_handle, agent);
}

fn push(ev: &'static str, summary: &str, body: &str, agent: Option<String>) {
    if cfg!(test) {
        return;
    }
    let (summary, body) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || crate::push::dispatch(ev, &summary, &body, agent.as_deref()));
}

/// Dashboard URL that opens `agent`'s chat (the web app reads `?agent=` on load).
pub fn chat_url(agent: &str) -> String {
    let enc: String =
        agent.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("http://127.0.0.1:{}/?agent={enc}", crate::util::port())
}

/// Click on a notification: the agent's chat on the dashboard; without an agent, its terminal.
fn open_target(agent: Option<&str>, focus_handle: Option<&str>) {
    if let Some(a) = agent {
        let _ = crate::platform::spawn_app(crate::platform::opener(), &[chat_url(a).into()]);
    } else if let Some(h) = focus_handle {
        let _ = crate::term::focus(h);
    }
}

fn desktop(summary: &str, body: &str, focus_handle: Option<String>, agent: Option<&str>) {
    let summary = summary.to_string();
    let body = body.to_string();
    let agent = agent.map(String::from);
    eprintln!("adash: notify: {summary} | {}", truncate(&body, 120));
    if let Ok(mut q) = PENDING.lock() {
        q.push((crate::util::now_ms(), summary.clone(), body.clone(), focus_handle.clone().unwrap_or_default()));
    }
    if cfg!(test) {
        return; // never pop desktop notifications from unit tests
    }
    // at most 16 notifications waiting for a click at once; beyond that, show without the action
    static OPEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if OPEN.load(std::sync::atomic::Ordering::Relaxed) >= 16 {
        crate::platform::notify(&summary, &body, None, agent.as_deref());
        return;
    }
    OPEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::thread::spawn(move || {
        // the action wait ends when the notification closes; bounded so threads cannot pile up
        if crate::platform::notify(&summary, &body, Some("열기"), agent.as_deref()) {
            open_target(agent.as_deref(), focus_handle.as_deref());
        }
        OPEN.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    });
}

#[derive(Default)]
pub struct Notifier {
    prev: Option<HashMap<String, String>>, // identity → state
    sent: HashSet<(String, i64)>,          // debounce: (identity, stateSince)
    last: HashMap<String, i64>,            // identity → last notification (ms)
}

/// A finished turn right after another notification of the same agent (a question answered
/// and done a moment later, a quick follow-up turn) is folded into it.
const FOLD_MS: i64 = 60_000;

impl Notifier {
    fn recent(&self, identity: &str) -> bool {
        self.last.get(identity).is_some_and(|t| crate::util::now_ms() - t < FOLD_MS)
    }

    /// Notify on poll-to-poll working → needs-human transitions (never on the first snapshot).
    pub fn observe(&mut self, snap: &Snapshot) -> Vec<Agent> {
        let mut fired = Vec::new();
        if let Some(prev) = &self.prev {
            // hidden projects and snoozed agents stay quiet (errors included)
            for a in snap.agents.iter().filter(|a| !a.hidden && a.snoozed_until.is_none()) {
                let was = prev.get(&a.identity).map(String::as_str);
                // a login prompt: once per prompt
                if a.state == "auth" && was.is_some_and(|w| w != "auth") && self.sent.insert((a.identity.clone(), a.state_since)) {
                    let t = format!("로그인 필요 · {} · {}", a.name, a.project);
                    let b = format!("{} 대시보드의 터미널 보기에서 로그인하세요.", a.auth.as_ref().and_then(|v| v["reason"].as_str()).unwrap_or(""));
                    desktop(&t, &b, a.handle.clone(), Some(&a.name));
                    push("error", &t, &b, Some(a.name.clone()));
                    self.last.insert(a.identity.clone(), crate::util::now_ms());
                    fired.push(a.clone());
                    continue;
                }
                // a provider error stopped the agent: once per error
                if a.state == "error" && was.is_some_and(|w| w != "error") && self.sent.insert((a.identity.clone(), a.state_since)) {
                    let e = a.last_error.clone().unwrap_or_default();
                    let t = format!("오류 · {} · {}{}", a.name, e.status.map(|s| format!("{s} ")).unwrap_or_default(), truncate(&e.message, 80));
                    let b = format!(
                        "{} · {} 에서 멈췄습니다. 대시보드에서 확인하세요.",
                        a.project,
                        [e.provider.as_deref(), e.model.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("/")
                    );
                    desktop(&t, &b, a.handle.clone(), Some(&a.name));
                    push("error", &t, &b, Some(a.name.clone()));
                    self.last.insert(a.identity.clone(), crate::util::now_ms());
                    fired.push(a.clone());
                    continue;
                }
                // entering "asking" always notifies (the previous poll may have seen it working)
                if a.state == "asking" && was.is_some_and(|w| w != "asking") && a.needs_human && self.sent.insert((a.identity.clone(), a.state_since)) {
                    let q = a.ask.as_ref().map(|q| q.question.clone()).unwrap_or_default();
                    let opts = a.ask.as_ref().map(|q| q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>().join(" / ")).unwrap_or_default();
                    let (t, b) = (
                        format!("질문 대기 · {} · {}", a.name, a.project),
                        truncate(&format!("{q}{}", if opts.is_empty() { String::new() } else { format!(" — {opts}") }), 160),
                    );
                    desktop(&t, &b, a.handle.clone(), Some(&a.name));
                    push("asking", &t, &b, Some(a.name.clone()));
                    self.last.insert(a.identity.clone(), crate::util::now_ms());
                    fired.push(a.clone());
                } else if was == Some("working")
                    && a.needs_human
                    && !a.quiet
                    && !self.recent(&a.identity)
                    && self.sent.insert((a.identity.clone(), a.state_since))
                {
                    let msg = truncate(a.last_message.trim(), 120);
                    let (t, b) = (format!("{} · {}", a.name, a.project), format!("{} — {}", a.state, msg));
                    desktop(&t, &b, a.handle.clone(), Some(&a.name));
                    // phones only hear about agents the user talked to in the last 2 h
                    if a.last_human_at.is_some_and(|h| crate::util::now_ms() - h < 2 * 3_600_000) {
                        push("waiting_after_human", &t, &b, Some(a.name.clone()));
                    }
                    self.last.insert(a.identity.clone(), crate::util::now_ms());
                    fired.push(a.clone());
                }
            }
        }
        self.prev = Some(snap.agents.iter().map(|a| (a.identity.clone(), a.state.clone())).collect());
        if self.sent.len() > 2000 {
            self.sent.clear();
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snoozed_agents_do_not_notify_even_on_errors() {
        let agent = |state: &str, snoozed: bool| Agent {
            identity: "/s/a.jsonl".into(),
            name: "A".into(),
            state: state.into(),
            state_since: 5,
            needs_human: !snoozed && state != "working",
            snoozed_until: snoozed.then_some(i64::MAX),
            ..Default::default()
        };
        let snap = |a: Agent| Snapshot { agents: vec![a], ..Default::default() };
        let mut n = Notifier::default();
        n.observe(&snap(agent("working", true)));
        assert!(n.observe(&snap(agent("error", true))).is_empty(), "snoozed error: quiet");
        let mut m = Notifier::default();
        m.observe(&snap(agent("working", false)));
        // the desktop/push side effects run; the fired list is what matters here
        assert_eq!(m.observe(&snap(agent("error", false))).len(), 1);
    }

    #[test]
    fn only_my_turn_notifies_and_a_quick_follow_up_folds_in() {
        let agent = |state: &str, since: i64, hidden: bool| Agent {
            identity: "/s/b.jsonl".into(),
            name: "B".into(),
            state: state.into(),
            state_since: since,
            needs_human: state != "working",
            hidden,
            ..Default::default()
        };
        let snap = |a: Agent| Snapshot { agents: vec![a], ..Default::default() };
        let mut n = Notifier::default();
        n.observe(&snap(agent("working", 1, false)));
        assert!(n.observe(&snap(agent("working", 1, false))).is_empty(), "still working: quiet");
        assert_eq!(n.observe(&snap(agent("waiting", 2, false))).len(), 1, "turn done: once");
        assert!(n.observe(&snap(agent("waiting", 2, false))).is_empty(), "same state: no repeat");
        n.observe(&snap(agent("working", 3, false)));
        assert!(n.observe(&snap(agent("waiting", 4, false))).is_empty(), "another turn within a minute folds in");
        n.last.clear();
        n.observe(&snap(agent("working", 5, false)));
        assert_eq!(n.observe(&snap(agent("waiting", 6, false))).len(), 1);
        // hidden project: quiet
        let mut h = Notifier::default();
        h.observe(&snap(agent("working", 1, true)));
        assert!(h.observe(&snap(agent("waiting", 2, true))).is_empty());
    }

    #[test]
    fn chat_url_opens_the_agent() {
        assert!(chat_url("PLANNER · grant").ends_with("/?agent=PLANNER%20%C2%B7%20grant"));
    }
}
