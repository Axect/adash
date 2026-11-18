//! Subagents an agent launched and that run after its own turn ended (omp `task` in the
//! background, Claude Code's Task tool). Their session files sit next to the parent's:
//! - omp: `<session>.jsonl` → `<session>/<Name>.jsonl` (first line a `title` record);
//! - Claude Code: `<id>.jsonl` → `<id>/subagents/*.jsonl` (sidechain records).
//!
//! A subagent runs while its last record is not a finished assistant turn, or for a minute after
//! it finished (the parent picks up the result and starts working again). A file untouched for
//! 30 minutes is done whatever it says (an aborted run).
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const GRACE_MS: i64 = 60_000;
const STALE_MS: i64 = 30 * 60_000;

/// `[{name, file, running, lastTool, startedAt, updatedAt}]`, None without subagent files.
pub fn of(kind: &str, session: &Path) -> Option<Value> {
    let dir = session.with_extension("");
    let (dir, check): (PathBuf, fn(&str) -> bool) = match kind {
        "omp" => (dir, |first| first.contains("\"type\":\"title\"")),
        "claude" => (dir.join("subagents"), |_| true),
        _ => return None,
    };
    let now = crate::util::now_ms();
    let mut out: Vec<Value> = vec![];
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(mut f) = std::fs::File::open(&p) else { continue };
        let mut head = [0u8; 256];
        let n = f.read(&mut head).unwrap_or(0);
        if !check(&String::from_utf8_lossy(&head[..n])) {
            continue;
        }
        let mt = crate::util::mtime_ms(&p);
        let started = e
            .metadata()
            .ok()
            .and_then(|m| m.created().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(mt);
        let (ended, tool) = last_state(kind, &mut f);
        let running = now - mt < STALE_MS && (!ended || now - mt < GRACE_MS);
        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        out.push(json!({"name": name, "file": p, "running": running, "lastTool": tool, "startedAt": started, "updatedAt": mt}));
    }
    if out.is_empty() {
        return None;
    }
    out.sort_by_key(|v| v["startedAt"].as_i64().unwrap_or(0));
    Some(json!(out))
}

/// (the last record is a finished assistant turn, the last tool called) from the file's tail.
fn last_state(kind: &str, f: &mut std::fs::File) -> (bool, Option<String>) {
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(256 * 1024)));
    let mut buf = String::new();
    let _ = f.read_to_string(&mut buf);
    let recs: Vec<Value> = buf.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let msg = |v: &Value| -> Option<Value> { (v["message"].is_object()).then(|| v["message"].clone()) };
    let last = recs.iter().rev().find_map(msg);
    let ended = last.as_ref().is_some_and(|m| {
        m["role"] == "assistant" && matches!(m["stopReason"].as_str().or(m["stop_reason"].as_str()), Some("stop" | "end_turn" | "stop_sequence"))
    });
    let _ = kind;
    let tool = recs.iter().rev().filter_map(msg).find_map(|m| {
        m["content"]
            .as_array()?
            .iter()
            .rev()
            .find(|c| matches!(c["type"].as_str(), Some("toolCall" | "tool_use")))
            .and_then(|c| c["name"].as_str().map(String::from))
    });
    (ended, tool)
}

#[cfg(test)]
mod tests {
    #[test]
    fn running_and_finished_subagents() {
        let dir = std::env::temp_dir().join(format!("adash-sub-{}", std::process::id()));
        let sess = dir.join("s1.jsonl");
        let sub = dir.join("s1");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&sess, "").unwrap();
        let title = "{\"type\":\"title\",\"v\":1}\n";
        std::fs::write(
            sub.join("Busy.jsonl"),
            format!("{title}{}\n", r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash"}]}}"#),
        )
        .unwrap();
        std::fs::write(
            sub.join("Done.jsonl"),
            format!("{title}{}\n", r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text"}]}}"#),
        )
        .unwrap();
        std::fs::write(sub.join("changes.jsonl"), "{\"type\":\"call\"}\n").unwrap();
        let v = super::of("omp", &sess).unwrap();
        let l = v.as_array().unwrap();
        assert_eq!(l.len(), 2, "changes.jsonl is not a subagent");
        let busy = l.iter().find(|s| s["name"] == "Busy").unwrap();
        assert_eq!((busy["running"].as_bool(), busy["lastTool"].as_str()), (Some(true), Some("bash")));
        // just finished: still counted for the grace minute
        assert_eq!(l.iter().find(|s| s["name"] == "Done").unwrap()["running"], true);
        assert!(super::of("omp", &dir.join("none.jsonl")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
