//! Terminals survive a restart of the tmux server (reboot): every background round saves the
//! adash terminals to `~/.local/share/adash/panes.json`; when the server came back under a new
//! pid without any of the saved terminals, each one is recreated with its old handle, directory,
//! session and window name. Agents whose session file still exists (and no other process holds
//! it) are resumed with their recorded flags; other terminals come back as plain shells.
use crate::collect::Snapshot;
use crate::term;
use crate::util::{home, now_ms, LockExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SavedPane {
    handle: String,
    session: String,
    window_name: String,
    cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_path: Option<PathBuf>,
    /// the agent process's argv (flags kept by `lifecycle::resume_command`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct Saved {
    server_pid: u32,
    saved_at: i64,
    panes: Vec<SavedPane>,
}

#[derive(Debug, PartialEq)]
enum Action {
    /// server down: leave the file alone
    Nothing,
    Save,
    Restore,
}

/// Restore only when the server restarted (other pid), none of the saved terminals is back and
/// this server pid was not restored already; otherwise save the current terminals.
fn decide(saved: Option<&Saved>, server_pid: Option<u32>, handles: &[String], restored_for: Option<u32>) -> Action {
    let Some(pid) = server_pid else { return Action::Nothing };
    match saved {
        Some(s) if s.server_pid != pid && restored_for != Some(pid) && !s.panes.is_empty() && !s.panes.iter().any(|p| handles.contains(&p.handle)) => {
            Action::Restore
        }
        _ => Action::Save,
    }
}

fn path() -> PathBuf {
    crate::platform::data_dir().join("panes.json")
}

struct State {
    /// server pid already restored in this process
    restored_for: Option<u32>,
    /// last written (server pid, panes): skip identical writes
    last: Option<(u32, Vec<SavedPane>)>,
    /// restored agent terminals whose agent is not mapped yet: (entry, restored at ms); keeps their
    /// resume details in the file until the collector sees the agent again
    pending: HashMap<String, (SavedPane, i64)>,
    /// agents held back after a restart: per harness, the one started first (its handle, start
    /// time) and the rest waiting until it shows it is signed in
    probe: HashMap<String, (String, i64)>,
    held: Vec<SavedPane>,
}

/// How long the first agent of a harness gets to show a sign-in screen before the rest follow.
const PROBE_MS: i64 = 20_000;

/// The restored agents waiting for their harness's first agent to be signed in:
/// [{handle, name, agentType, waitingFor}].
pub fn held() -> Vec<serde_json::Value> {
    let st = STATE.plock();
    st.held
        .iter()
        .map(|p| {
            let kind = p.agent_type.clone().unwrap_or_default();
            serde_json::json!({"handle": p.handle, "name": p.name, "agentType": kind, "waitingFor": st.probe.get(&kind).map(|x| x.0.clone())})
        })
        .collect()
}

fn start(p: &SavedPane, st: &mut State) {
    let cmd = resume(p);
    let cwd = if Path::new(&p.cwd).is_dir() { PathBuf::from(&p.cwd) } else { home() };
    let what = cmd.as_deref().unwrap_or("shell");
    match term::create(&cwd, &p.session, &p.window_name, cmd.as_deref(), Some(&p.handle)) {
        Ok(_) => {
            eprintln!("adash: restored terminal {} ({}/{}) in {}: {what}", p.handle, p.session, p.window_name, cwd.display());
            if cmd.is_some() {
                st.pending.insert(p.handle.clone(), (p.clone(), now_ms()));
            }
        }
        Err(e) => eprintln!("adash: restoring terminal {} ({}/{}) failed: {e}", p.handle, p.session, p.window_name),
    }
}

/// Held agents of harnesses whose first agent is past its sign-in check: start them. A first
/// agent at a sign-in screen keeps the rest waiting (the user signs in there once); one whose
/// terminal is gone hands the check to the next.
fn release(st: &mut State) {
    let now = now_ms();
    let kinds: Vec<String> = st.probe.keys().cloned().collect();
    for kind in kinds {
        let (h, at) = st.probe[&kind].clone();
        let screen = term::screen(&h);
        let gone = screen.is_err();
        let at_login = screen.as_ref().is_ok_and(|l| crate::auth::detect(&kind, l).is_some());
        if !gone && (at_login || now - at < PROBE_MS) {
            continue;
        }
        st.probe.remove(&kind);
        let (go, keep): (Vec<SavedPane>, Vec<SavedPane>) =
            std::mem::take(&mut st.held).into_iter().partition(|p| p.agent_type.as_deref() == Some(kind.as_str()));
        st.held = keep;
        let mut go = go.into_iter();
        if gone {
            // the probe disappeared: the next one checks instead
            if let Some(next) = go.next() {
                start(&next, st);
                st.probe.insert(kind.clone(), (next.handle.clone(), now));
                st.held.extend(go);
            }
            continue;
        }
        eprintln!("adash: {kind} signed in; restoring the held agents");
        for p in go {
            start(&p, st);
        }
    }
}

static STATE: LazyLock<Mutex<State>> =
    LazyLock::new(|| Mutex::new(State { restored_for: None, last: None, pending: HashMap::new(), probe: HashMap::new(), held: vec![] }));

/// How long a restored agent keeps its saved resume details without showing up as an agent.
const PENDING_MS: i64 = 120_000;

/// One background round: restore after a server restart, else save the current terminals.
pub fn tick(snap: &Snapshot) {
    let Ok(panes) = term::panes() else { return };
    let server_pid = term::server_pid();
    let saved: Option<Saved> = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok());
    let handles: Vec<String> = panes.iter().map(|p| p.handle.clone()).collect();
    let mut st = STATE.plock();
    release(&mut st);
    match decide(saved.as_ref(), server_pid, &handles, st.restored_for) {
        Action::Nothing => return,
        Action::Restore => {
            let pid = server_pid.unwrap_or(0);
            st.restored_for = Some(pid);
            // after a reboot a harness's sign-in may have expired: start one agent per harness
            // first; the others follow once it shows no sign-in screen (else they would all stop
            // at the same screen)
            for p in &saved.as_ref().map(|s| s.panes.clone()).unwrap_or_default() {
                match p.agent_type.clone().filter(|_| resume(p).is_some()) {
                    Some(kind) if st.probe.contains_key(&kind) => st.held.push(p.clone()),
                    Some(kind) => {
                        start(p, &mut st);
                        st.probe.insert(kind, (p.handle.clone(), now_ms()));
                    }
                    None => start(p, &mut st),
                }
            }
        }
        Action::Save => {}
    }
    // save what runs now (after a restore: the recreated terminals)
    let Some(pid) = server_pid else { return };
    let Ok(panes) = term::panes() else { return };
    let procs = crate::session::scan_procs();
    let now = now_ms();
    let entries: Vec<SavedPane> = panes
        .iter()
        .map(|p| {
            let agent = snap.agents.iter().find(|a| a.handle.as_deref() == Some(p.handle.as_str()));
            let mut e = SavedPane {
                handle: p.handle.clone(),
                session: p.session.clone(),
                window_name: p.window_name.clone(),
                cwd: p.cwd.clone(),
                ..Default::default()
            };
            match agent {
                Some(a) => {
                    st.pending.remove(&p.handle);
                    e.agent_type = Some(a.agent_type.clone());
                    // only trusted mappings: a guessed session must not be resumed
                    e.session_path = a.session_path.clone().filter(|_| a.mapping_exact);
                    e.args = procs.get(&p.handle).and_then(|v| v.iter().find(|x| x.kind == a.agent_type)).map(|x| x.args.clone());
                    e.name = Some(a.name.clone());
                }
                None => {
                    if let Some((prev, _)) = st.pending.get(&p.handle).filter(|(_, at)| now - at < PENDING_MS) {
                        e.agent_type = prev.agent_type.clone();
                        e.session_path = prev.session_path.clone();
                        e.args = prev.args.clone();
                        e.name = prev.name.clone();
                    }
                }
            }
            e
        })
        .collect();
    st.pending.retain(|_, (_, at)| now - *at < PENDING_MS);
    let mut entries = entries;
    entries.extend(st.held.iter().cloned());
    if st.last.as_ref().is_some_and(|(lp, le)| *lp == pid && *le == entries) {
        return;
    }
    let out = Saved { server_pid: pid, saved_at: now, panes: entries.clone() };
    match crate::util::atomic_write(&path(), serde_json::to_string_pretty(&out).unwrap_or_default().as_bytes()) {
        Ok(()) => st.last = Some((pid, entries)),
        Err(e) => eprintln!("adash: saving {}: {e}", path().display()),
    }
}

/// The resume command of a saved agent terminal, when its session can be resumed safely.
fn resume(p: &SavedPane) -> Option<String> {
    let kind = p.agent_type.as_deref()?;
    let file = p.session_path.as_deref().filter(|f| f.exists())?;
    if crate::session::procs_using_session(file, &crate::lifecycle::session_id(kind, file)) {
        return None;
    }
    crate::lifecycle::resume_command(kind, file, p.args.as_deref().unwrap_or(&[]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_once_after_a_server_restart_without_its_terminals() {
        let pane = |h: &str| SavedPane { handle: h.into(), ..Default::default() };
        let saved = Saved { server_pid: 100, saved_at: 1, panes: vec![pane("t_aaaaaaaa"), pane("t_bbbbbbbb")] };
        let none: Vec<String> = vec![];
        assert_eq!(decide(Some(&saved), None, &none, None), Action::Nothing, "server down");
        assert_eq!(decide(Some(&saved), Some(100), &none, None), Action::Save, "same server");
        assert_eq!(decide(Some(&saved), Some(200), &none, None), Action::Restore, "new server, terminals gone");
        assert_eq!(decide(Some(&saved), Some(200), &none, Some(200)), Action::Save, "never twice for one server pid");
        assert_eq!(decide(Some(&saved), Some(200), &["t_bbbbbbbb".to_string()], None), Action::Save, "a saved terminal is back");
        assert_eq!(decide(Some(&Saved { server_pid: 100, saved_at: 1, panes: vec![] }), Some(200), &none, None), Action::Save, "nothing to restore");
        assert_eq!(decide(None, Some(200), &none, None), Action::Save, "first run");
    }
}
