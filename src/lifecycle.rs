//! Close an agent from the dashboard and reopen it later on the same session.
//! Exit commands per harness: omp `/exit` (its "Exit the application" command; Ctrl+D also
//! exits), claude `/exit`, codex `/quit`, agy Ctrl+C twice; then the terminal is closed. The
//! session file stays; a closed agent's launch details go to `closed_agents.json` for `reopen`.
use crate::collect::{Agent, Snapshot};
use crate::term;
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type ApiErr = (u16, Value);

fn store_path() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-closed-{}.json", std::process::id()));
    }
    crate::util::state_dir().join("closed_agents.json")
}

fn load_store() -> Value {
    std::fs::read_to_string(store_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
}

pub(crate) fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Session id a harness resumes by: claude = file stem, codex = trailing uuid of the rollout
/// name, agy = db stem.
pub(crate) fn session_id(kind: &str, file: &Path) -> String {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if kind == "codex" && stem.len() >= 36 {
        return stem[stem.len() - 36..].to_string();
    }
    stem
}

/// The shell command that resumes `file` (with the recorded model/config flags).
pub fn resume_command(kind: &str, file: &Path, args: &[String]) -> Option<String> {
    crate::harness::get(kind)?.resume_command(file, args)
}

fn exit_keys(kind: &str) -> Vec<(&'static str, bool)> {
    crate::harness::get(kind).map(|h| h.exit_keys()).unwrap_or_else(|| vec![("\u{3}", false), ("\u{3}", false)])
}

fn alive(pid: u32) -> bool {
    crate::platform::alive(pid)
}

fn err(code: u16, reason: &str, extra: Value) -> ApiErr {
    let mut v = json!({"error": reason, "reason": reason});
    if let Value::Object(m) = extra {
        for (k, x) in m {
            v[k] = x;
        }
    }
    (code, v)
}

/// (pane identity, session file) of closed agents recorded under their pane key.
pub fn pane_rekeys() -> Vec<(String, String)> {
    load_store()
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v["sessionPath"].as_str().filter(|p| *p != k && !k.starts_with('/')).map(|p| (k.clone(), p.to_string())))
        .collect()
}

/// `POST /api/agents/{name}/close {mode, confirmWorking?, discardDraft?}`.
pub fn close(snap: &Snapshot, a: &Agent, j: &Value) -> Result<Value, ApiErr> {
    let mode = j["mode"].as_str().unwrap_or("graceful");
    if !matches!(mode, "graceful" | "force") {
        return Err(err(400, "mode", json!({"error": "mode must be graceful|force"})));
    }
    let h = a.handle.clone().ok_or_else(|| err(409, "no_terminal", json!({})))?;
    // only agent panes: a process of a known harness runs in this terminal
    let procs = crate::session::scan_procs().remove(&h).unwrap_or_default();
    if procs.is_empty() {
        return Err(err(409, "not_agent_pane", json!({"error": "no agent process in this terminal"})));
    }
    if matches!(a.state.as_str(), "working" | "asking") && j["confirmWorking"] != true {
        return Err(err(409, &a.state, json!({"state": a.state})));
    }
    let draft = a.draft.as_ref().and_then(|d| d["text"].as_str()).filter(|t| !t.trim().is_empty()).map(String::from);
    if let (Some(d), false) = (&draft, j["discardDraft"] == true) {
        return Err(err(409, "draft", json!({"draft": d})));
    }
    // remember how to start it again (name/role stay in the registry, keyed by session identity)
    let main = procs.iter().find(|p| p.kind == a.agent_type).or(procs.first()).cloned().unwrap();
    if let Some(file) = &a.session_path {
        let mut st = load_store();
        st[a.identity.as_str()] = json!({
            "name": a.name, "role": a.role, "agentType": a.agent_type, "project": a.project, "cwd": main.cwd, "args": main.args,
            "sessionPath": file, "mapping": a.mapping_confidence, "closedAt": now_ms(), "model": a.model,
        });
        let _ = crate::util::atomic_write(&store_path(), serde_json::to_string_pretty(&st).unwrap().as_bytes());
    }
    let _ = snap;
    let lk = term::send_lock(&h);
    let _g = lk.plock();
    let mut steps = vec![];
    // keep the agent's last screen for later diagnosis
    if term::snapshot(&h, "close").is_some() {
        steps.push("snapshot");
    }
    if mode == "graceful" {
        if a.state == "working" {
            let _ = term::interrupt(&h, &a.agent_type);
            steps.push("interrupt");
            std::thread::sleep(Duration::from_millis(1500));
        }
        if draft.is_some() {
            let _ = term::send_text(&h, "\u{15}");
            steps.push("clear_draft");
        }
        for (k, enter) in exit_keys(&a.agent_type) {
            let r = if enter { term::send_line(&h, k) } else { term::send_text(&h, k) };
            r.map_err(|e| err(502, "send_failed", json!({"error": e})))?;
            std::thread::sleep(Duration::from_millis(300));
        }
        steps.push("exit_command");
        let t0 = now_ms();
        while now_ms() - t0 < 10_000 && procs.iter().any(|p| alive(p.pid)) {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    let exited_before_close = procs.iter().all(|p| !alive(p.pid));
    if term::exists(&h) {
        if let Err(e) = term::close(&h) {
            if term::exists(&h) {
                return Err(err(502, "close_failed", json!({"error": e})));
            }
        }
        steps.push("terminal_close");
    }
    let t0 = now_ms();
    while now_ms() - t0 < 5_000 && (procs.iter().any(|p| alive(p.pid)) || term::exists(&h)) {
        std::thread::sleep(Duration::from_millis(300));
    }
    let exited = procs.iter().all(|p| !alive(p.pid));
    let gone = !term::exists(&h);
    Ok(
        json!({"name": a.name, "ok": exited && gone, "mode": mode, "steps": steps, "exitedGracefully": exited_before_close, "processExited": exited, "terminalClosed": gone, "reopenable": a.session_path.is_some()}),
    )
}

/// Launch details for a closed/offline agent: the dashboard's close record, else the
/// analytics session (exact identity = session file) with default flags.
fn closed_entry(key: &str) -> Option<Value> {
    let st = load_store();
    if let Some(v) = st.get(key) {
        return Some(v.clone());
    }
    let by_name = st.as_object()?.values().filter(|v| v["name"] == key).max_by_key(|v| v["closedAt"].as_i64().unwrap_or(0)).cloned();
    if by_name.is_some() {
        return by_name;
    }
    let db = crate::analytics::open_read().ok()?;
    db.conn
        .query_row(
            "SELECT file, agent_type, cwd, project, COALESCE(json_extract(names, '$[#-1]'), json_extract(titles, '$[#-1]'), project) FROM sessions WHERE file = ?1 OR EXISTS (SELECT 1 FROM json_each(names) WHERE value = ?1) ORDER BY last_ts DESC LIMIT 1",
            [key],
            |r| Ok(json!({"sessionPath": r.get::<_, String>(0)?, "agentType": r.get::<_, String>(1)?, "cwd": r.get::<_, Option<String>>(2)?, "project": r.get::<_, Option<String>>(3)?, "name": r.get::<_, Option<String>>(4)?.unwrap_or_else(|| key.to_string()), "args": []})),
        )
        .ok()
}

/// `POST /api/agents/{name}/reopen` (name or session identity of an offline agent).
pub fn reopen(snap: &Snapshot, key: &str) -> Result<Value, ApiErr> {
    let e = closed_entry(key).ok_or_else(|| err(404, "unknown", json!({"error": format!("no closed or offline agent {key}")})))?;
    let file = PathBuf::from(e["sessionPath"].as_str().unwrap_or(""));
    let kind = e["agentType"].as_str().unwrap_or("omp").to_string();
    if !file.exists() {
        return Err(err(409, "session_missing", json!({"error": format!("session file {} is gone", file.display())})));
    }
    // two writers would corrupt the session: refuse when any live process has it (also
    // processes outside adash terminals)
    if snap.agents.iter().any(|a| a.session_path.as_deref() == Some(file.as_path())) || crate::session::procs_using_session(&file, &session_id(&kind, &file)) {
        return Err(err(409, "already_open", json!({"error": "a live agent already uses this session"})));
    }
    let args: Vec<String> = e["args"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
    let cmd = resume_command(&kind, &file, &args).ok_or_else(|| err(501, "harness", json!({"error": format!("cannot reopen {kind}")})))?;
    let cwd = e["cwd"].as_str().filter(|c| !c.is_empty()).ok_or_else(|| err(409, "no_cwd", json!({"error": "working directory unknown"})))?;
    let name = e["name"].as_str().unwrap_or(key).to_string();
    let project = e["project"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(String::from)
        .unwrap_or_else(|| crate::meta::Meta::load().project_for_path(cwd).unwrap_or_else(|| crate::collect::project_name("", cwd)));
    let handle = term::create(Path::new(cwd), &project, &name, Some(&cmd), None).map_err(|x| err(502, "create_failed", json!({"error": x})))?;
    Ok(json!({"ok": true, "name": name, "handle": handle, "command": cmd, "sessionPath": file}))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn resume_commands_keep_model_flags_and_quote() {
        let args: Vec<String> =
            ["omp", "--config", "fresh.yml", "--model", "anthropic/claude-opus-5-5", "--prompt", "x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            super::resume_command("omp", Path::new("/s/a b.jsonl"), &args).unwrap(),
            "omp '--config' 'fresh.yml' '--model' 'anthropic/claude-opus-5-5' --resume '/s/a b.jsonl'"
        );
        assert_eq!(super::resume_command("claude", Path::new("/p/0f1e.jsonl"), &[]).unwrap(), "claude --resume '0f1e'");
        let codex = Path::new("/c/rollout-2026-10-31T01-02-03-01a18e7b-14de-72af-bfcf-dfd164b211eb.jsonl");
        assert_eq!(super::resume_command("codex", codex, &[]).unwrap(), "codex resume '01a18e7b-14de-72af-bfcf-dfd164b211eb'");
    }
}
