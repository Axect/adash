//! omp "btw" side questions: `<session-stem>/btw-history/entry-<id>.json` next to the session
//! jsonl, `{id, question, answer, status, createdAt, updatedAt, leafId}`.
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub id: String,
    pub question: String,
    pub answer: String,
    /// omp status ("complete", "error", "pending", …)
    pub status: String,
    pub created: i64,
    pub updated: i64,
    /// the session record the question was asked at
    pub leaf: Option<String>,
    pub error: Option<String>,
    /// follow-up questions asked in the btw panel (`followUps`), same shape
    pub follow_ups: Vec<Entry>,
}

impl Entry {
    /// Question status for the UI: pending | answered | error.
    pub fn q_status(&self) -> &'static str {
        match self.status.as_str() {
            "complete" | "completed" | "done" | "answered" => "answered",
            "error" | "failed" | "aborted" | "cancelled" => "error",
            _ => "pending",
        }
    }
}

pub fn dir(session: &Path) -> PathBuf {
    session.with_extension("").join("btw-history")
}

/// Change signature of the directory (entries count + newest mtime), "" when absent.
pub fn sig(session: &Path) -> String {
    let Ok(rd) = std::fs::read_dir(dir(session)) else { return String::new() };
    let (mut n, mut m) = (0, 0i64);
    for e in rd.flatten() {
        n += 1;
        m = m.max(crate::util::mtime_ms(&e.path()));
    }
    format!("{n}.{m}")
}

pub fn parse(v: &Value) -> Option<Entry> {
    let ts = |k: &str| {
        v[k].as_i64().or_else(|| {
            v[k].as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| v[k].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()))
        })
    };
    let question = v["question"].as_str()?.to_string();
    let created = ts("createdAt").unwrap_or(0);
    Some(Entry {
        id: v["id"].as_str().map(String::from).unwrap_or_else(|| created.to_string()),
        question,
        answer: v["answer"].as_str().unwrap_or("").to_string(),
        status: v["status"].as_str().unwrap_or("").to_string(),
        created,
        updated: ts("updatedAt").unwrap_or(created),
        leaf: v["leafId"].as_str().map(String::from),
        error: v["error"].as_str().map(String::from).or_else(|| v["error"]["message"].as_str().map(String::from)),
        follow_ups: v["followUps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse)
            .enumerate()
            .map(|(i, mut f)| {
                f.id = format!("{}.f{}", v["id"].as_str().unwrap_or(""), i + 1);
                f
            })
            .collect(),
    })
}

/// What the terminal shows of omp's btw UI: None (closed), or Some(follow-up input open).
/// The answer box is "╭─ /btw <question>" with "… to close"; the history panel "╭─ BTW
/// history"; the follow-up input line "Follow up:".
pub fn screen_state(lines: &[String]) -> Option<bool> {
    let start = lines.iter().rposition(|l| {
        let t = l.trim_start();
        t.starts_with("╭─ /btw ") || t.starts_with("╭─ BTW history")
    })?;
    let rest = &lines[start..];
    if !rest.iter().any(|l| l.contains("to close") || l.contains(" close ·") || l.contains("cancel")) {
        return None;
    }
    Some(rest.iter().any(|l| l.contains("Follow up:")))
}

/// All entries of a session, oldest first.
pub fn load(session: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir(session)) else { return vec![] };
    let mut v: Vec<Entry> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<Value>(&s).ok())
        .filter_map(|j| parse(&j))
        .collect();
    v.sort_by_key(|e| e.created);
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn screen_state_reads_the_btw_box() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::screen_state(&l(&[" OK.", "╭─ /btw say A ──╮", "│ A │", "│ c to copy · f to follow up · x to close │", "╰──╯"])), Some(false));
        assert_eq!(super::screen_state(&l(&["╭─ BTW history ──╮", "│ Follow up: │", "│ send · cancel │", "╰──╯"])), Some(true));
        assert_eq!(super::screen_state(&l(&[" OK.", "────"])), None);
    }

    #[test]
    fn entry_fields_and_status() {
        let j = serde_json::json!({"question": "How?", "answer": "So.", "status": "complete", "createdAt": "1793389819355", "updatedAt": 1793389833797i64, "id": "15ba", "leafId": "0171",
            "followUps": [{"question": "and?", "answer": "Six.", "status": "complete", "createdAt": 1793389840000i64}]});
        let e = super::parse(&j).unwrap();
        assert_eq!((e.follow_ups.len(), e.follow_ups[0].id.as_str(), e.follow_ups[0].answer.as_str()), (1, "15ba.f1", "Six."));
        assert_eq!((e.created, e.updated, e.q_status(), e.leaf.as_deref()), (1793389819355, 1793389833797, "answered", Some("0171")));
        assert_eq!(super::parse(&serde_json::json!({"question": "x", "status": "streaming"})).unwrap().q_status(), "pending");
        assert_eq!(super::dir(std::path::Path::new("/s/a.jsonl")), std::path::PathBuf::from("/s/a/btw-history"));
    }
}
