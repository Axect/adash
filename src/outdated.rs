//! Agents still running a harness that has since been updated (`agents[].outdated`), and
//! restarting them on the same session (`POST /api/restart-outdated`, `adash restart --outdated`).
//!
//! Only certain signs count (Linux `/proc`): the process's executable was replaced on disk
//! (`/proc/<pid>/exe` ends in " (deleted)": omp, agy, the Codex binary), or Claude Code runs
//! `~/.local/share/claude/versions/<v>` while the `claude` link points at another version. The
//! installed version comes from `<harness> --version` (cached per binary mtime).
use crate::collect::Agent;
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// `--version` output, first version-looking token, per (binary, mtime).
fn installed_version(bin: &Path) -> Option<String> {
    static C: LazyLock<Mutex<HashMap<PathBuf, (i64, Option<String>)>>> = LazyLock::new(Default::default);
    let mt = crate::util::mtime_ms(bin);
    if let Some((m, v)) = C.plock().get(bin) {
        if *m == mt {
            return v.clone();
        }
    }
    let out = crate::platform::timeout(10).arg(bin).arg("--version").output().ok();
    let v = out.and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).into_owned();
        s.split(|c: char| c.is_whitespace() || c == '/').find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.')).map(String::from)
    });
    C.plock().insert(bin.to_path_buf(), (mt, v.clone()));
    v
}

fn bin_name(kind: &str) -> &str {
    if kind == "antigravity" {
        "agy"
    } else {
        kind
    }
}

/// The executable link of a process and of its direct children.
fn exes(pid: u32) -> Vec<String> {
    let mut v = vec![];
    let mut pids = vec![pid];
    if let Ok(c) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
        pids.extend(c.split_whitespace().filter_map(|x| x.parse::<u32>().ok()));
    }
    for p in pids {
        if let Ok(l) = std::fs::read_link(format!("/proc/{p}/exe")) {
            v.push(l.to_string_lossy().into_owned());
        }
    }
    v
}

/// Whether a running harness process is older than the installed one: {running, installed,
/// why}. `exe` = the process's (and children's) executable links, `current` = what the
/// harness's command resolves to now.
pub fn judge(kind: &str, exe: &[String], current: Option<&Path>, installed: Option<String>) -> Option<Value> {
    if kind == "claude" {
        let cur = current?.to_string_lossy().into_owned();
        let run = exe.iter().find(|e| e.contains("/claude/versions/"))?;
        let run = run.trim_end_matches(" (deleted)");
        if run == cur {
            return None;
        }
        let ver = |p: &str| p.rsplit('/').next().unwrap_or("").to_string();
        return Some(json!({"running": ver(run), "installed": ver(&cur), "why": "version"}));
    }
    exe.iter().find(|e| e.ends_with(" (deleted)"))?;
    Some(json!({"running": null, "installed": installed, "why": "replaced"}))
}

/// handle → outdated info for live agents (rechecked at most once a minute).
pub fn scan(agents: &[Agent]) -> HashMap<String, Value> {
    static LAST: LazyLock<Mutex<(i64, HashMap<String, Value>)>> = LazyLock::new(Default::default);
    if cfg!(not(target_os = "linux")) {
        return HashMap::new();
    }
    {
        let l = LAST.plock();
        if now_ms() - l.0 < 60_000 {
            return l.1.clone();
        }
    }
    let procs = crate::session::scan_procs();
    let mut out = HashMap::new();
    for a in agents {
        let Some(h) = &a.handle else { continue };
        let Some(p) = procs.get(h).and_then(|v| v.iter().find(|p| p.kind == a.agent_type)) else { continue };
        let bin = crate::launch::find_bin(bin_name(&a.agent_type)).and_then(|b| std::fs::canonicalize(b).ok());
        let installed = || bin.as_deref().and_then(installed_version);
        let ex = exes(p.pid);
        let inst = if a.agent_type == "claude" { None } else { installed() };
        if let Some(mut v) = judge(&a.agent_type, &ex, bin.as_deref(), inst) {
            // omp records the version it runs on each message
            if a.agent_type == "omp" && v["running"].is_null() {
                v["running"] = json!(a.session_path.as_deref().and_then(omp_runtime));
            }
            out.insert(h.clone(), v);
        }
    }
    *LAST.plock() = (now_ms(), out.clone());
    out
}

/// The omp version of the last message record (`runtime.version`).
fn omp_runtime(f: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(f).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(64 * 1024))).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    buf.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| v["runtime"]["version"].as_str().map(String::from))
}

/// Agents to restart later (when their turn ends), by name.
pub static WHEN_IDLE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// What a bulk restart does with one agent: "now", "later" (working), or a skip reason.
pub fn plan(a: &Agent, when_idle: bool) -> &'static str {
    let draft = a.draft.as_ref().and_then(|d| d["text"].as_str()).is_some_and(|t| !t.trim().is_empty());
    match a.state.as_str() {
        "asking" | "trust" => "skip_asking",
        _ if draft => "skip_draft",
        "working" if when_idle => "later",
        "working" => "skip_working",
        _ => "now",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn only_certain_signs_count() {
        let cur = Path::new("/h/.local/share/claude/versions/2.1.322");
        let old = vec!["/h/.local/share/claude/versions/2.1.319".to_string()];
        assert_eq!(judge("claude", &old, Some(cur), None).unwrap()["running"], "2.1.319");
        assert!(judge("claude", &["/h/.local/share/claude/versions/2.1.322".into()], Some(cur), None).is_none());
        // omp/agy/codex: the executable was replaced; a start time alone is not proof
        assert_eq!(judge("omp", &["/h/.bun/bin/omp (deleted)".into()], None, Some("18.18.6".into())).unwrap()["installed"], "18.18.6");
        assert!(judge("omp", &["/h/.bun/bin/omp".into()], None, Some("18.18.6".into())).is_none());
        assert!(judge("codex", &["/usr/bin/node".into(), "/n/vendor/codex (deleted)".into()], None, None).is_some(), "the Codex binary is a child of node");
    }

    #[test]
    fn bulk_restart_plan() {
        let a = |state: &str, draft: bool| Agent { state: state.into(), draft: draft.then(|| json!({"text": "half"})), ..Default::default() };
        assert_eq!(plan(&a("done", false), false), "now");
        assert_eq!(plan(&a("working", false), true), "later");
        assert_eq!(plan(&a("working", false), false), "skip_working");
        assert_eq!(plan(&a("asking", false), true), "skip_asking");
        assert_eq!(plan(&a("idle", true), true), "skip_draft");
    }
}
