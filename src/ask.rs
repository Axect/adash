//! Open interactive questions (omp `ask`, claude `AskUserQuestion`) and answering them
//! through terminal keystrokes.
use serde::Serialize;
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AskQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<AskOption>,
    pub multi: bool,
    /// index of the option the agent recommends (omp `recommended`); the TUI starts there
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<usize>,
    /// omp `header`: the tab / review-page label (falls back to the id)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

impl AskQuestion {
    /// Tab label as omp draws it: header or id, cut to 16 columns with "…".
    pub fn tab_label(&self) -> String {
        let base = self.header.as_deref().map(str::trim).filter(|h| !h.is_empty()).unwrap_or(&self.id);
        let mut w = 0;
        let mut out = String::new();
        for c in base.chars() {
            let cw = if crate::util::is_wide(c) { 2 } else { 1 };
            if w + cw > 16 {
                out.pop();
                out.push('…');
                return out;
            }
            w += cw;
            out.push(c);
        }
        out
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    /// mirror of questions[0] (kept for older UI code)
    pub question: String,
    pub options: Vec<AskOption>,
    pub multi: bool,
    pub questions: Vec<AskQuestion>,
    pub asked_at: i64,
    /// "question" | "permission"
    pub ask_kind: &'static str,
    /// tool call id (answer verification); not part of the API
    #[serde(skip)]
    pub call_id: String,
}

fn ts_of(v: &Value) -> i64 {
    v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()).unwrap_or(0)
}

fn from_questions(qs: &[Value], call_id: &str, at: i64, multi_key: &str) -> Option<Ask> {
    let questions: Vec<AskQuestion> = qs
        .iter()
        .enumerate()
        .map(|(i, q)| AskQuestion {
            id: q["id"].as_str().or(q["header"].as_str()).map(String::from).unwrap_or_else(|| format!("q{}", i + 1)),
            question: q["question"].as_str().unwrap_or("").to_string(),
            options: q["options"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|o| match o {
                            Value::String(s) => AskOption { label: s.clone(), description: None },
                            _ => AskOption { label: o["label"].as_str().unwrap_or("").to_string(), description: o["description"].as_str().map(String::from) },
                        })
                        .collect()
                })
                .unwrap_or_default(),
            multi: q[multi_key].as_bool().unwrap_or(false),
            recommended: q["recommended"].as_u64().map(|n| n as usize),
            header: q["header"].as_str().map(String::from),
        })
        .collect();
    let first = questions.first()?.clone();
    Some(Ask {
        question: first.question,
        options: first.options,
        multi: first.multi,
        questions,
        asked_at: at,
        ask_kind: "question",
        call_id: call_id.to_string(),
    })
}

/// Feed one session record; keeps `open` = the latest unanswered ask.
pub fn feed(kind: &str, r: &Value, open: &mut Option<Ask>) {
    match kind {
        "omp" => {
            if r["type"] != "message" {
                return;
            }
            let m = &r["message"];
            match m["role"].as_str() {
                Some("assistant") => {
                    for c in m["content"].as_array().into_iter().flatten() {
                        if c["type"] == "toolCall" && c["name"] == "ask" {
                            let qs = c["arguments"]["questions"].as_array().cloned().unwrap_or_default();
                            *open = from_questions(&qs, c["id"].as_str().unwrap_or(""), ts_of(&r["timestamp"]), "multi");
                        }
                    }
                }
                Some("toolResult") => {
                    if open.as_ref().is_some_and(|a| m["toolCallId"].as_str() == Some(a.call_id.as_str())) {
                        *open = None;
                    }
                }
                // a new user turn means the question was abandoned (interrupt / typed over)
                Some("user") => *open = None,
                _ => {}
            }
        }
        "claude" => {
            let content = &r["message"]["content"];
            for c in content.as_array().into_iter().flatten() {
                if r["type"] == "assistant" && c["type"] == "tool_use" && c["name"] == "AskUserQuestion" {
                    let qs = c["input"]["questions"].as_array().cloned().unwrap_or_default();
                    *open = from_questions(&qs, c["id"].as_str().unwrap_or(""), ts_of(&r["timestamp"]), "multiSelect");
                }
                if r["type"] == "user" && c["type"] == "tool_result" && open.as_ref().is_some_and(|a| c["tool_use_id"].as_str() == Some(a.call_id.as_str())) {
                    *open = None;
                }
            }
            if r["type"] == "user" && content.is_string() {
                *open = None;
            }
        }
        _ => {}
    }
}

/// What the bottom of an omp screen shows for an open ask.
#[derive(Debug, PartialEq)]
pub enum Screen {
    /// the dialog: `title` = question title (whitespace removed), `review` = Submit tab,
    /// `lines` = dialog lines (borders trimmed)
    Dialog {
        title: String,
        review: bool,
        lines: Vec<String>,
    },
    /// "Custom answer:" / "Note for …" text prompt open on top of the dialog
    Prompt {
        custom: bool,
        lines: Vec<String>,
    },
    /// dialog hidden with Alt+H ("Ask hidden: …")
    Hidden,
    None,
}

fn inner(l: &str) -> String {
    l.trim().trim_start_matches(['│', '├', '╭', '╰']).trim_end_matches(['│', '┤', '╮', '╯']).trim().to_string()
}

pub fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && *c != '…').collect()
}

/// Parse the live ask UI. The transcript's tool-call preview ("╭─── Ask N questions") also
/// lists every question, so only the dialog border "╭─ Ask " counts.
pub fn screen(tail: &[String]) -> Screen {
    let dlg = tail.iter().rposition(|l| l.trim_start().starts_with("╭─ Ask"));
    let prompt = tail.iter().rposition(|l| l.trim_start().starts_with("╭─ Custom answer:") || l.trim_start().starts_with("╭─ Note for "));
    let hidden = tail.iter().rposition(|l| l.trim_start().starts_with("Ask hidden:"));
    let last = [dlg, prompt, hidden].into_iter().flatten().max();
    match last {
        None => Screen::None,
        Some(i) if Some(i) == hidden => Screen::Hidden,
        Some(i) if Some(i) == prompt => {
            Screen::Prompt { custom: tail[i].contains("Custom answer:"), lines: tail[i..].iter().map(|l| inner(l)).filter(|l| !l.is_empty()).collect() }
        }
        Some(i) => {
            let lines: Vec<String> = tail[i + 1..].iter().take_while(|l| !l.trim_start().starts_with('╰')).map(|l| inner(l)).collect();
            let raw: Vec<&String> = tail[i + 1..].iter().take_while(|l| !l.trim_start().starts_with('╰')).collect();
            // header: [tab bar] title lines … up to the first ├ separator
            let head_end = raw.iter().position(|l| l.trim_start().starts_with('├')).unwrap_or(raw.len());
            let head = &lines[..head_end];
            let has_tabs = head.first().is_some_and(|l| l.ends_with("Submit"));
            let title_lines = if has_tabs { &head[1..] } else { head };
            let review = title_lines.iter().any(|l| l == "Review answers");
            Screen::Dialog { title: squash(&title_lines.join("")), review, lines }
        }
    }
}

/// Questions and options read off the screen (no session record needed). omp prints the
/// ask call as a "╭─── Ask N questions" block: per question a "├─── [id] · options:N" bar,
/// the question line(s), option rows with a radio/checkbox glyph and "↳" descriptions.
/// Without that block the live "╭─ Ask" dialog gives the current question.
pub fn dialog_ask(tail: &[String]) -> Option<Vec<AskQuestion>> {
    const MARKS: &[char] = &['☐', '☑', '☒', '○', '●', '◯', '◉', '\u{f096}', '\u{f14a}', '\u{f046}', '\u{f10c}', '\u{f192}', '\u{f111}'];
    let option = |l: &str| -> Option<String> {
        let t = l.trim_start_matches('\u{f054}').trim();
        let rest = t.strip_prefix(MARKS)?;
        let label = rest.trim().trim_end_matches(" (Recommended)").trim().to_string();
        (!label.is_empty() && label != "Other (type your own)").then_some(label)
    };
    let preview = tail.iter().rposition(|l| l.trim_start().starts_with("╭───") && l.contains(" Ask"));
    let mut qs: Vec<AskQuestion> = Vec::new();
    if let Some(i) = preview {
        for raw in tail[i + 1..].iter().take_while(|l| !l.trim_start().starts_with('╰')) {
            let l = inner(raw);
            if raw.trim_start().starts_with('├') {
                let id = l.trim_start_matches('─').trim().trim_start_matches('[').split(']').next().unwrap_or("").to_string();
                qs.push(AskQuestion {
                    header: None,
                    id: if id.is_empty() { format!("q{}", qs.len() + 1) } else { id },
                    question: String::new(),
                    options: vec![],
                    multi: false,
                    recommended: None,
                });
            } else if let Some(q) = qs.last_mut() {
                if let Some(label) = option(&l) {
                    q.options.push(AskOption { label, description: None });
                } else if let Some(d) = l.strip_prefix('↳') {
                    if let Some(o) = q.options.last_mut() {
                        let d = d.trim();
                        o.description = Some(match o.description.take() {
                            Some(p) => format!("{p} {d}"),
                            None => d.to_string(),
                        });
                    }
                } else if q.options.is_empty() && !l.is_empty() {
                    q.question = if q.question.is_empty() { l.clone() } else { format!("{} {l}", q.question) };
                }
            }
        }
    }
    if qs.iter().all(|q| q.options.is_empty()) {
        let Screen::Dialog { title, lines, .. } = screen(tail) else { return None };
        let mut q = AskQuestion { header: None, id: "q1".into(), question: title, options: vec![], multi: false, recommended: None };
        for l in &lines {
            if let Some(label) = option(l) {
                q.options.push(AskOption { label, description: None });
            }
        }
        if let Some(first) = lines.iter().find(|l| !l.is_empty() && !l.ends_with("Submit") && option(l).is_none()) {
            q.question = first.clone();
        }
        qs = vec![q];
    }
    qs.retain(|q| !q.options.is_empty());
    (!qs.is_empty()).then_some(qs)
}

/// Which question the dialog shows (index into `qs`), by its (possibly wrapped/cut) title.
pub fn current_question(title: &str, qs: &[AskQuestion]) -> Option<usize> {
    if title.chars().count() < 4 {
        return None;
    }
    let hits: Vec<usize> = qs
        .iter()
        .enumerate()
        .filter(|(_, q)| {
            let t = squash(&q.question);
            t == title || (title.chars().count() >= 12 && t.starts_with(title))
        })
        .map(|(i, _)| i)
        .collect();
    (hits.len() == 1).then(|| hits[0])
}

/// Label of the option row under the cursor (U+F054), without its checkbox/radio glyph and
/// the "(Recommended)" tag.
pub fn cursor_label(lines: &[String]) -> Option<String> {
    let row = lines.iter().find(|l| l.starts_with('\u{f054}'))?;
    let t = row.trim_start_matches('\u{f054}').trim().trim_start_matches(['\u{f096}', '\u{f14a}', '\u{f046}', '\u{f10c}', '\u{f192}', '\u{f111}']).trim();
    Some(t.trim_end_matches(" (Recommended)").trim().to_string())
}

/// A cursor row (possibly wrapped or cut with "…") shows option `want`.
pub fn label_matches(shown: &str, want: &str) -> bool {
    let (s, w) = (squash(shown), squash(want.trim_end_matches(" (Recommended)")));
    !s.is_empty() && (s == w || (s.chars().count() >= 4 && w.starts_with(&s)))
}

/// Review page answers in question order (`k. <tab label>: answer`). A long answer wraps over
/// several screen lines and omp may cut it with "…"; continuation lines are joined.
pub fn review_answers(lines: &[String], qs: &[AskQuestion]) -> Vec<Option<String>> {
    let head = |k: usize| format!("{}. {}:", k + 1, qs[k].tab_label());
    let starts: Vec<Option<usize>> = (0..qs.len()).map(|k| lines.iter().position(|l| l.starts_with(&head(k)))).collect();
    (0..qs.len())
        .map(|k| {
            let i = starts[k]?;
            let mut text = lines[i][head(k).len()..].trim().to_string();
            for l in &lines[i + 1..] {
                let stop = l.trim().is_empty() || l.starts_with('\u{f054}') || starts.iter().flatten().any(|&j| lines[j] == *l);
                if stop {
                    break;
                }
                text.push(' ');
                text.push_str(l.trim());
            }
            Some(text)
        })
        .collect()
}

/// Does a review-page answer show `want`? Whitespace (wrapping) is ignored; an answer that omp
/// cut with "…" must be a prefix of `want`.
pub fn review_matches(shown: &str, want: &str) -> bool {
    let shown = shown.replace(" (Recommended)", "");
    let t = shown.trim_end();
    let cut = t.ends_with('…') || t.ends_with("...");
    let s = squash(t.trim_end_matches("...").trim_end_matches('…'));
    let w = squash(&want.replace(" (Recommended)", ""));
    if cut {
        !s.is_empty() && w.starts_with(&s)
    } else {
        s == w
    }
}

/// Review-page form of an answer: labels joined by ", "; custom text in curly quotes
/// (multi-select custom text is `Other: “…”`).
pub fn review_text(q: &AskQuestion, ans: &Answer) -> String {
    match ans {
        Answer::Text(t) if q.multi => format!("Other: “{}”", expected_text(q, ans)),
        Answer::Text(_) => format!("“{}”", expected_text(q, ans)),
        _ => expected_text(q, ans),
    }
}

/// A few dialog lines for error messages (what the user would see).
pub fn excerpt(sc: &Screen, tail: &[String]) -> String {
    let lines: Vec<String> = match sc {
        Screen::Dialog { lines, .. } | Screen::Prompt { lines, .. } => lines.clone(),
        _ => tail.iter().rev().take(8).rev().map(|l| inner(l)).collect(),
    };
    lines.iter().filter(|l| !l.is_empty() && !l.chars().all(|c| c == '─')).take(14).map(|l| crate::util::truncate(l, 90)).collect::<Vec<_>>().join("\n")
}

/// What the review page / toolResult should say for an answer: "물", "치즈, 계란", custom text.
pub fn expected_text(q: &AskQuestion, ans: &Answer) -> String {
    match ans {
        Answer::One(i) => q.options.get(*i).map(|o| o.label.clone()).unwrap_or_default(),
        Answer::Many(v) => {
            let mut s = v.clone();
            s.sort_unstable();
            s.dedup();
            s.iter().filter_map(|i| q.options.get(*i).map(|o| o.label.clone())).collect::<Vec<_>>().join(", ")
        }
        Answer::Text(t) => t.replace(['\r', '\n'], " ").trim().to_string(),
    }
}

/// Does a toolResult `details` entry record this answer?
pub fn result_matches(detail: &Value, q: &AskQuestion, ans: &Answer) -> bool {
    let sel: Vec<String> = detail["selectedOptions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    match ans {
        Answer::Text(t) => detail["customInput"].as_str().map(str::trim) == Some(t.replace(['\r', '\n'], " ").trim()),
        _ => sel.join(", ") == expected_text(q, ans),
    }
}

pub enum Answer {
    One(usize),
    Many(Vec<usize>),
    Text(String),
}

/// Keystroke batches (each sent separately) for one question, starting from cursor row `cur`.
/// Rows are the options followed by "Other (type your own)". The final Enter selects (single),
/// moves on (multi: "next"), or submits when it is the only question.
pub fn keys(q: &AskQuestion, cur: usize, ans: &Answer) -> Result<Vec<(String, bool)>, String> {
    let n = q.options.len();
    let ask = q;
    let down = "\x1b[B".to_string();
    let up = "\x1b[A".to_string();
    let mv = |from: usize, to: usize| -> Vec<(String, bool)> {
        if to >= from {
            vec![(down.clone(), false); to - from]
        } else {
            vec![(up.clone(), false); from - to]
        }
    };
    let mut out = Vec::new();
    match ans {
        Answer::One(i) => {
            if ask.multi {
                return Err("multi-select question: use optionIndices".into());
            }
            if *i >= n {
                return Err(format!("optionIndex {i} out of range (0..{n})"));
            }
            out.extend(mv(cur, *i));
            out.push((String::new(), true));
        }
        Answer::Many(idx) => {
            if !ask.multi {
                return Err("single-select question: use optionIndex".into());
            }
            let mut sorted = idx.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if sorted.is_empty() || sorted.iter().any(|i| *i >= n) {
                return Err(format!("optionIndices must be non-empty and in 0..{n}"));
            }
            let mut pos = cur;
            for i in sorted {
                out.extend(mv(pos, i));
                out.push((" ".into(), false));
                pos = i;
            }
            out.push((String::new(), true));
        }
        Answer::Text(t) => {
            let t = t.replace(['\r', '\n'], " ");
            if t.trim().is_empty() {
                return Err("empty text".into());
            }
            out.extend(mv(cur, n)); // "Other (type your own)"
            out.push((String::new(), true));
            out.push((t, true));
        }
    }
    Ok(out)
}

/// One answered question: chosen option labels and/or custom text ("기타").
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AnswerItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub question: String,
    pub picked: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Answers recorded in an ask result. omp: `details` (`results[]` for several questions, else
/// one object); Claude: the tool_result text `User has answered your questions: "Q"="A", …`.
pub fn answer_items(v: &Value) -> Vec<AnswerItem> {
    let one = |d: &Value| -> Option<AnswerItem> {
        Some(AnswerItem {
            id: d["id"].as_str().map(String::from),
            question: d["question"].as_str()?.to_string(),
            picked: d["selectedOptions"].as_array().map(|x| x.iter().filter_map(|o| o.as_str().map(String::from)).collect()).unwrap_or_default(),
            text: d["customInput"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(String::from),
        })
    };
    if let Some(r) = v["results"].as_array() {
        return r.iter().filter_map(one).collect();
    }
    if v.is_object() {
        return one(v).into_iter().collect();
    }
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    static QA: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#""([^"]+)"="([^"]*)""#).unwrap());
    if !text.contains("answered your question") {
        return vec![];
    }
    // Claude does not say which part was typed; everything counts as picked
    QA.captures_iter(&text)
        .map(|c| AnswerItem {
            id: None,
            question: c[1].to_string(),
            picked: c[2].split(", ").map(String::from).filter(|x| !x.is_empty()).collect(),
            text: None,
        })
        .collect()
}

/// One line per question: `질문 → 답1, 답2` (search / copy text).
pub fn answer_text(items: &[AnswerItem]) -> String {
    items
        .iter()
        .map(|i| {
            let mut a = i.picked.clone();
            a.extend(i.text.clone());
            format!("{} → {}", i.question.trim(), if a.is_empty() { "(답 없음)".to_string() } else { a.join(", ") })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Call ids answered from the dashboard (seeded from analytics at start, added on each answer):
/// the chat labels those answers "대시보드", the rest "터미널".
pub static DASHBOARD_ANSWERS: LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, multi: bool) -> Value {
        json!({"type":"message","timestamp":"2026-10-29T04:35:24.524Z","message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"…"},
            {"type":"toolCall","id":id,"name":"ask","arguments":{"i":"점심 메뉴 선택","questions":[{"id":"lunch","question":"점심 메뉴는?","options":[{"label":"김밥"},{"label":"라면"},{"label":"비빔밥"}],"multi":multi}]}}]}})
    }

    fn run(recs: &[Value]) -> Option<Ask> {
        let mut open = None;
        for r in recs {
            feed("omp", r, &mut open);
        }
        open
    }

    #[test]
    fn open_ask_is_detected_with_question_and_options() {
        let a = run(&[call("t1", false)]).expect("open ask");
        assert_eq!(a.question, "점심 메뉴는?");
        assert_eq!(a.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["김밥", "라면", "비빔밥"]);
        assert!(!a.multi);
        assert_eq!(a.asked_at, 1793248524524);
    }

    #[test]
    fn answered_ask_is_closed() {
        let res = json!({"type":"message","message":{"role":"toolResult","toolCallId":"t1","toolName":"ask","content":[{"type":"text","text":"User selected: 라면"}],"isError":false}});
        assert!(run(&[call("t1", false), res]).is_none());
        // a result for another call keeps it open
        let other = json!({"type":"message","message":{"role":"toolResult","toolCallId":"zz","toolName":"read"}});
        assert!(run(&[call("t1", false), other]).is_some());
    }

    #[test]
    fn interrupted_ask_is_closed() {
        // Esc in the Ask UI: error toolResult; a typed-over new prompt: user message
        let cancelled = json!({"type":"message","message":{"role":"toolResult","toolCallId":"t1","toolName":"ask","content":[{"type":"text","text":"Ask tool was cancelled by the user"}],"isError":true}});
        assert!(run(&[call("t1", false), cancelled]).is_none());
        let user = json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"그만"}]}});
        assert!(run(&[call("t1", false), user]).is_none());
    }

    #[test]
    fn keystrokes_for_answers() {
        let a = run(&[call("t1", false)]).unwrap();
        let q = &a.questions[0];
        let k = keys(q, 0, &Answer::One(1)).unwrap();
        assert_eq!(k, vec![("\x1b[B".into(), false), (String::new(), true)]);
        let k = keys(q, 0, &Answer::Text("보리차".into())).unwrap();
        assert_eq!(k.len(), 3 + 2); // 3 downs to "Other", Enter, text+Enter
        assert!(keys(q, 0, &Answer::One(3)).is_err());
        // cursor starts on a recommended option: move up from there
        assert_eq!(keys(q, 2, &Answer::One(0)).unwrap(), vec![("\x1b[A".into(), false), ("\x1b[A".into(), false), (String::new(), true)]);
        let m = run(&[call("t2", true)]).unwrap();
        let k = keys(&m.questions[0], 0, &Answer::Many(vec![2, 0])).unwrap();
        assert_eq!(k, vec![(" ".into(), false), ("\x1b[B".into(), false), ("\x1b[B".into(), false), (" ".into(), false), (String::new(), true)]);
    }

    /// Real 3-question ask recorded from omp 18.13 (2026-10-29): mixed single/multi, recommended.
    fn multi_call() -> Value {
        json!({"type":"message","timestamp":"2026-10-29T09:52:40.000Z","message":{"role":"assistant","content":[
            {"type":"toolCall","id":"toolu_01TZ","name":"ask","arguments":{"i":"Asking three questions at once","questions":[
                {"id":"drink","question":"음료는?","options":[{"label":"물"},{"label":"커피"}],"recommended":1},
                {"id":"toppings","question":"토핑은?","options":[{"label":"치즈"},{"label":"햄"},{"label":"계란"}],"multi":true},
                {"id":"size","question":"크기는?","options":[{"label":"S"},{"label":"M"},{"label":"L"}]}]}}]}})
    }

    #[test]
    fn multi_question_ask_keeps_every_question() {
        let a = run(&[multi_call()]).unwrap();
        assert_eq!(a.questions.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(), ["drink", "toppings", "size"]);
        assert_eq!(a.questions[0].recommended, Some(1));
        assert!(a.questions[1].multi && !a.questions[2].multi);
        assert_eq!(a.question, "음료는?", "legacy mirror of questions[0]");
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn survey_questions() -> Vec<AskQuestion> {
        let q = |id: &str, header: &str, text: &str, n: usize, multi: bool| AskQuestion {
            id: id.into(),
            header: Some(header.into()),
            question: text.into(),
            options: (0..n).map(|i| AskOption { label: format!("{id}-{i}"), description: None }).collect(),
            multi,
            recommended: None,
        };
        vec![
            q("n1", "신규 1/2", "10-28 전수 조사 이후 새로 찾은 공고입니다(AJO·INSPIRE 전수 + 독일·한국 기관 직접 조사). 지원할 것을 고르세요(미선택 = 안 함). 최우선은 다음 질문에서.", 6, true),
            q("n2", "신규 2/2", "계속. 지원할 것 선택(미선택 = 안 함). ①②는 비AJO 추천서 요구라 한도(지금 7/10)를 1씩 씁니다.", 5, true),
            q("top", "최우선", "위에서 고른 것 중 최우선(맞춤형 서류)으로 올릴 것이 있으면 Other에 이름을 적어 주세요.", 1, false),
        ]
    }

    /// Screens captured from an omp ask: the transcript preview
    /// above lists every question, the dialog below shows one, its title wrapped when narrow.
    #[test]
    fn dialog_question_is_read_from_the_dialog_not_the_preview() {
        let qs = survey_questions();
        let narrow = lines(&[
            "╭─── Ask 3 questions ──────",
            "├─── [n2] · multi · options:5 ───",
            "│  계속. 지원할 것 선택(미선택 = 안 함). ①②는 비AJO 추천서 요구라 한도(지금 7/10)를 1씩 씁니다.",
            "╰──────",
            "╭─ Ask ──────────────",
            "│  신규 1/2    신규 2/2    최우선    Submit",
            "│ 10-28 전수 조사 이후 새로 찾은 공고입니다(AJO·INSPIRE 전수 + 독일·한국 기관 직접",
            "│ 조사). 지원할 것을 고르세요(미선택 = 안 함). 최우선은 다음 질문에서.   │",
            "├──────────────",
            "│ \u{f054} \u{f096} EHU 빌바오 이론물리 (ES)",
            "│   \u{f096} Other (type your own)",
            "├──────────────",
            "│ \u{f1050} toggle · next · ↑/↓ move · question · cancel · Alt+H hide",
            "╰──────────────",
        ]);
        let Screen::Dialog { title, review, .. } = screen(&narrow) else { panic!("dialog expected") };
        assert!(!review);
        assert_eq!(current_question(&title, &qs), Some(0));
        // title cut with an ellipsis after MAX_HEADER_ROWS still identifies the question
        assert_eq!(current_question(&squash("계속. 지원할 것 선택(미선택 = 안 함). ①②는 비AJO…"), &qs), Some(1));
        assert_eq!(current_question("", &qs), None);

        let mut prompt = narrow.clone();
        prompt.extend(lines(&[
            "╭─ Custom answer: 10-28 전수 조사 이후 새로 찾은 공고입니다",
            "│ >",
            "│ or Ctrl+Q submit  cancel  Ctrl+G external editor",
            "╰────",
        ]));
        assert!(matches!(screen(&prompt), Screen::Prompt { custom: true, .. }));
        let mut hidden = narrow[..4].to_vec();
        hidden.push("Ask hidden: 10-28 전수 조사 이후 … (+2 more) · Alt+H show".into());
        assert_eq!(screen(&hidden), Screen::Hidden);
        assert_eq!(screen(&narrow[..4]), Screen::None);
    }

    /// A long multi-select answer wraps and omp cuts it with "…".
    #[test]
    fn review_page_long_answers_are_joined_and_cut_answers_match_by_prefix() {
        let mk = |id: &str| AskQuestion { id: id.into(), header: None, question: format!("{id}?"), options: vec![], multi: true, recommended: None };
        let qs = vec![mk("fixes"), mk("note")];
        let lines: Vec<String> = [
            "fixes    note    Submit",
            "Review answers",
            "1. fixes: 1. 관측 계획으로 넘어가는 문장, 2. 연구 목표를 한 문장으로, 3. 선행 연구와의 차이,",
            "4. 동기서 …",
            "2. note: “짧은 메모”",
            "",
            "\u{f054} Submit",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = review_answers(&lines, &qs);
        let full = "1. 관측 계획으로 넘어가는 문장, 2. 연구 목표를 한 문장으로, 3. 선행 연구와의 차이, 4. 동기서 첫 문단 수정, 5. 기간, 6. 예산, 7. 클러스터 계산 자원을 이름·규모로";
        assert!(review_matches(got[0].as_deref().unwrap(), full));
        assert!(!review_matches(got[0].as_deref().unwrap(), "1. 다른 답, 2. …"));
        assert!(review_matches(got[1].as_deref().unwrap(), "“짧은 메모”"));
        assert!(!review_matches(got[1].as_deref().unwrap(), "“짧은 메모 더”")); // not cut: must be equal
    }

    #[test]
    fn review_page_uses_tab_labels_and_quotes_custom_text() {
        let qs = survey_questions();
        let review = lines(&[
            "╭─ Ask ───",
            "│  신규 1/2    신규 2/2    최우선    Submit",
            "│ Review answers",
            "├───",
            "│ 1. 신규 1/2: n1-0, n1-2",
            "│ 2. 신규 2/2: n2-1 (Recommended), Other: “Bonn 문의”",
            "│ 3. 최우선: “EHU 빌바오”",
            "│",
            "│ \u{f054} Submit",
            "╰───",
        ]);
        let Screen::Dialog { review: true, lines: ls, .. } = screen(&review) else { panic!("review expected") };
        let got: Vec<String> = review_answers(&ls, &qs).into_iter().map(Option::unwrap).collect();
        assert!(review_matches(&got[0], &review_text(&qs[0], &Answer::Many(vec![2, 0]))));
        assert!(!review_matches(&got[1], &review_text(&qs[1], &Answer::Many(vec![1]))));
        assert!(review_matches(&got[2], &review_text(&qs[2], &Answer::Text("EHU 빌바오".into()))));
        assert_eq!(review_text(&qs[1], &Answer::Text("Bonn 문의".into())), "Other: “Bonn 문의”");
        assert_eq!(qs[0].tab_label(), "신규 1/2");
        let long = AskQuestion { header: Some("아주아주긴헤더라벨입니다".into()), ..qs[0].clone() };
        assert_eq!(long.tab_label(), "아주아주긴헤더…");
        let a = run(&[multi_call()]).unwrap();
        let detail = json!({"id":"toppings","selectedOptions":["치즈","계란"]});
        assert!(result_matches(&detail, &a.questions[1], &Answer::Many(vec![0, 2])));
        assert!(!result_matches(&detail, &a.questions[1], &Answer::Many(vec![0])));
        assert!(result_matches(&json!({"customInput":"보리차"}), &a.questions[0], &Answer::Text("보리차".into())));
    }

    #[test]
    fn screen_ask_block_gives_every_question_with_options() {
        let tail: Vec<String> = [
            "╭─── Ask 2 questions ───╮",
            "├─── [astro] · options:2 ───┤",
            "│  astro 제목 후보입니다. │",
            "│  \u{f10c} Primordial black holes, axions │",
            "│    ↳ 제 추천. │",
            "│  \u{f10c} Dark matter from data │",
            "├─── [base] · options:1 ───┤",
            "│  base 제목 후보입니다. │",
            "│  \u{f10c} Physics sets the conditions │",
            "╰──────╯",
            "╭─ Ask ───╮",
            "│  astro    base    Submit │",
            "╰──────╯",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let qs = super::dialog_ask(&tail).unwrap();
        assert_eq!(qs.iter().map(|q| (q.id.as_str(), q.options.len())).collect::<Vec<_>>(), [("astro", 2), ("base", 1)]);
        assert_eq!(qs[0].question, "astro 제목 후보입니다.");
        assert_eq!(qs[0].options[0].description.as_deref(), Some("제 추천."));
    }
}
