//! Derived /api/state document (contract in SPEC.md).
use crate::collect::Snapshot;
use crate::meta::{Meta, ProjectMeta};
use crate::threads::Thread;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const FORMULA: &str = "(impact + 2*leverage) / cost";

/// Thread statuses that need a human decision.
pub fn thread_needs_human(t: &Thread) -> bool {
    matches!(t.status.as_str(), "held_possible_draft" | "delivery_unconfirmed" | "escalated" | "failed") || (t.status == "answered" && t.from == "human")
}

/// The one counting rule (top bar, mobile tab, page title, triage header):
/// 내 차례 = 질문 + 미확인 답변 + 기타 대기. Snoozed agents count nowhere.
/// - asking: open question (permission prompts count as other)
/// - unread: answered since my last message / ack
/// - error: the last turn ended on a provider/API error (`lastError`)
/// - other: permission/approval prompts, blocked agents, review threads that need me
///   (held relay, draft-blocked delivery, escalation, failure, unconfirmed delivery) and magi
///   instructions held by a draft
pub fn counts(snap: &Snapshot, threads: &[Thread]) -> Value {
    // snoozed agents and agents of hidden projects count nowhere
    let live = |a: &&crate::collect::Agent| a.snoozed_until.is_none() && !a.hidden;
    let permission = |a: &&crate::collect::Agent| a.ask.as_ref().is_some_and(|q| q.ask_kind == "permission");
    let asking: Vec<&str> = snap.agents.iter().filter(live).filter(|a| a.state == "asking" && !permission(a)).map(|a| a.name.as_str()).collect();
    let error: Vec<&str> = snap.agents.iter().filter(live).filter(|a| a.state == "error").map(|a| a.name.as_str()).collect();
    let unread: Vec<&str> =
        snap.agents.iter().filter(live).filter(|a| a.state != "asking" && a.state != "error" && a.needs_human && a.unread).map(|a| a.name.as_str()).collect();
    let mut other: Vec<Value> = snap
        .agents
        .iter()
        .filter(live)
        .filter(|a| (a.state == "asking" && permission(a)) || (a.state == "blocked" && !a.unread))
        .map(|a| json!({"kind": "agent", "ref": a.name, "why": if a.state == "asking" { "permission" } else { a.state.as_str() }}))
        .collect();
    // magi instructions held at the source (draft in its input box)
    other.extend(
        crate::magi::RunState::load_all()
            .into_iter()
            .filter(|r| r.status == "held" && r.held_reason.as_deref().is_some_and(|h| h.contains("draft")))
            .map(|r| json!({"kind": "magi", "ref": r.id, "why": "held_draft"})),
    );
    other.extend(
        threads
            .iter()
            .filter(|t| thread_needs_human(t) && !(t.status == "answered" && t.from == "human"))
            .map(|t| json!({"kind": "thread", "ref": t.id, "why": t.status})),
    );
    let working = snap.agents.iter().filter(|a| a.state == "working").count();
    json!({
        "mine": asking.len() + unread.len() + error.len() + other.len(),
        "asking": asking.len(), "unread": unread.len(), "error": error.len(), "other": other.len(), "working": working,
        "refs": {"asking": asking, "unread": unread, "error": error, "other": other},
    })
}

pub fn build(snap: &Snapshot, meta: &Meta, threads: &[Thread]) -> Value {
    struct P {
        paths: Vec<String>,
        agents: Vec<String>,
        working: usize,
        needs: usize,
        wait: f64,
        prog: Vec<f64>,
    }
    let mut projects: BTreeMap<String, P> = BTreeMap::new();
    let blank = || P { paths: vec![], agents: vec![], working: 0, needs: 0, wait: 0.0, prog: vec![] };
    for w in &snap.worktrees {
        let p = projects.entry(w.project.clone()).or_insert_with(blank);
        if !p.paths.contains(&w.path) {
            p.paths.push(w.path.clone());
        }
    }
    for a in &snap.agents {
        let p = projects.entry(a.project.clone()).or_insert_with(blank);
        if !p.paths.contains(&a.worktree_path) {
            p.paths.push(a.worktree_path.clone());
        }
        p.agents.push(a.name.clone());
        if a.state == "working" {
            p.working += 1;
        }
        if a.needs_human {
            p.needs += 1;
            p.wait += a.waited_min;
        }
        if let Some(pr) = &a.progress {
            if pr.total > 0 {
                p.prog.push(pr.done as f64 / pr.total as f64);
            }
        }
    }
    for (name, pm) in &meta.projects {
        let p = projects.entry(name.clone()).or_insert_with(blank);
        for x in &pm.paths {
            if !p.paths.contains(x) {
                p.paths.push(x.clone());
            }
        }
    }
    let max_wait = projects.values().map(|p| p.wait).fold(0.0, f64::max);
    let default_pm = ProjectMeta::default();
    let score_of = |name: &str| meta.projects.get(name).unwrap_or(&default_pm).score();
    let round = |x: f64| (x * 100.0).round() / 100.0;

    let projects_json: Vec<Value> = projects
        .iter()
        .map(|(name, p)| {
            let pm = meta.projects.get(name).unwrap_or(&default_pm);
            json!({
                "name": name, "paths": p.paths, "goal": pm.goal,
                "impact": pm.impact, "leverage": pm.leverage, "cost": pm.cost,
                "score": round(pm.score()), "deadline": pm.deadline, "note": pm.note,
                "counts": {"working": p.working, "needsHuman": p.needs},
                "humanWaitMin": round(p.wait),
                "progress": if p.prog.is_empty() { Value::Null } else { json!(round(p.prog.iter().sum::<f64>() / p.prog.len() as f64)) },
                "bottleneck": max_wait > 0.0 && p.wait == max_wait,
                "agents": p.agents,
            })
        })
        .collect();

    let mut queue: Vec<(f64, i64, Value)> = Vec::new();
    for a in snap.agents.iter().filter(|a| a.needs_human) {
        let bonus = meta.agents.get(&a.name).map(|m| m.priority_bonus).unwrap_or(0.0);
        let sc = round(score_of(&a.project) + bonus);
        let mut item = json!({"kind": "agent", "ref": a.name, "project": a.project, "title": a.title, "since": a.state_since, "score": sc});
        if a.state == "asking" {
            item["asking"] = json!(true);
        }
        queue.push((sc, a.state_since, item));
    }
    for t in meta.human_tasks.iter().filter(|t| !t.done) {
        let sc = round(score_of(&t.project));
        let since = t
            .due
            .as_deref()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis())
            .unwrap_or(0);
        queue.push((sc, since, json!({"kind": "humanTask", "ref": t.id, "project": t.project, "title": t.title, "since": since, "score": sc})));
    }
    for t in threads.iter().filter(|t| thread_needs_human(t)) {
        let proj = snap.agents.iter().find(|a| a.name == t.to).map(|a| a.project.clone()).unwrap_or_default();
        let sc = round(score_of(&proj));
        let since = t.events.last().map(|e| e.at).unwrap_or(t.created_at);
        queue.push((
            sc,
            since,
            json!({"kind": "thread", "ref": t.id, "project": proj, "title": format!("{} → {}: {}", t.from, t.to, t.status), "since": since, "score": sc}),
        ));
    }
    // open questions block the agent outright: always ahead of score ordering
    let asking = |v: &Value| v["asking"] == true;
    queue.sort_by(|a, b| asking(&b.2).cmp(&asking(&a.2)).then(b.0.partial_cmp(&a.0).unwrap()).then(a.1.cmp(&b.1)));

    let counts = counts(snap, threads);
    json!({
        "generatedAt": snap.at,
        "counts": counts,
        "priorityFormula": FORMULA,
        "agents": snap.agents,
        "projects": projects_json,
        "queue": queue.into_iter().map(|q| q.2).collect::<Vec<_>>(),
        "threads": threads.iter().map(Thread::summary).collect::<Vec<_>>(),
        "humanTasks": meta.human_tasks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_follow_the_one_rule() {
        let mk = |name: &str, state: &str, needs: bool, unread: bool| crate::collect::Agent {
            name: name.into(),
            state: state.into(),
            needs_human: needs,
            unread,
            ..Default::default()
        };
        let mut snoozed = mk("S", "done", true, true);
        snoozed.snoozed_until = Some(i64::MAX);
        let mut perm = mk("P", "asking", true, false);
        perm.ask = Some(crate::ask::Ask {
            questions: vec![],
            question: "bash 승인 대기".into(),
            options: vec![],
            multi: false,
            asked_at: 0,
            ask_kind: "permission",
            call_id: String::new(),
            intro: vec![],
        });
        let snap = Snapshot {
            agents: vec![
                mk("Q", "asking", true, false),
                mk("U", "done", true, true),
                mk("ACKED", "done", true, false), // answered and already looked at: not mine
                mk("W", "working", false, false),
                snoozed,
                perm,
            ],
            ..Default::default()
        };
        let mut snap = snap;
        let mut e = mk("E", "error", true, true);
        e.last_error = Some(crate::session::ErrInfo { status: Some(403), message: "Request not allowed".into(), ..Default::default() });
        snap.agents.push(e);
        let c = counts(&snap, &[]);
        assert_eq!((c["error"].as_u64(), c["refs"]["error"][0].as_str()), (Some(1), Some("E")), "an errored agent is its own item, not unread/other");
        assert_eq!(
            (c["mine"].as_u64(), c["asking"].as_u64(), c["unread"].as_u64(), c["other"].as_u64(), c["working"].as_u64()),
            (Some(4), Some(1), Some(1), Some(1), Some(1))
        );
    }
}

#[cfg(test)]
mod hidden_tests {
    #[test]
    fn hidden_projects_count_nowhere() {
        let mk = |name: &str, project: &str, hidden: bool| crate::collect::Agent {
            name: name.into(),
            project: project.into(),
            state: "asking".into(),
            needs_human: true,
            hidden,
            ..Default::default()
        };
        let snap = crate::collect::Snapshot { agents: vec![mk("A", "shown", false), mk("B", "quiet", true)], ..Default::default() };
        let c = super::counts(&snap, &[]);
        assert_eq!((c["asking"].as_u64(), c["mine"].as_u64()), (Some(1), Some(1)));
        assert_eq!(c["refs"]["asking"][0], "A");
    }
}
