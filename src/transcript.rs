//! Chat transcripts for the overlay: incremental, READ-ONLY parsing of session files
//! (omp / claude / codex), terminal-read fallback for everything else.
use crate::collect::Agent;
use crate::term;
use crate::util::LockExt;
use crate::util::{truncate, Res};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Serialize, Clone, Debug)]
pub struct ToolRef {
    pub name: String,
    pub summary: String,
    /// path-like arguments (path/file_path/cwd fields, `/`-tokens of commands) for path resolution
    #[serde(skip)]
    pub paths: Vec<String>,
}

/// Path-like arguments of a tool call (test helper and path-resolution input).
#[cfg(test)]
pub(crate) fn tool_paths_of(name: &str, args: &Value) -> Vec<String> {
    tool_ref(name, String::new(), args).paths
}

fn tool_ref(name: &str, summary: String, args: &Value) -> ToolRef {
    let mut paths = Vec::new();
    let mut push_tokens = |s: &str| {
        let mut after_cd = false;
        for t in s.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ';' | '(' | ')' | '=' | ',' | '&' | '|')) {
            if t.is_empty() {
                continue;
            }
            // `cd <dir>` names a directory even without a slash; mark it as one ("dir/")
            let cd_target = after_cd && !t.starts_with('-');
            after_cd = t == "cd" || t == "pushd";
            if cd_target && !t.contains("://") && t.len() < 400 && paths.len() < 32 {
                paths.push(if t.ends_with('/') { t.to_string() } else { format!("{t}/") });
            } else if t.contains('/') && !t.contains("://") && t.len() < 400 && paths.len() < 32 {
                paths.push(t.to_string());
            }
        }
    };
    match args {
        Value::Object(o) => {
            for (k, v) in o {
                match v {
                    Value::String(s) if matches!(k.as_str(), "path" | "file_path" | "cwd" | "dir" | "directory" | "command" | "cmd" | "pattern" | "input") => {
                        push_tokens(s)
                    }
                    Value::Array(a) if matches!(k.as_str(), "paths" | "files") => a.iter().filter_map(|x| x.as_str()).for_each(&mut push_tokens),
                    _ => {}
                }
            }
        }
        Value::String(s) => push_tokens(s),
        _ => {}
    }
    ToolRef { name: name.into(), summary, paths }
}

#[derive(Serialize, Clone, Debug)]
pub struct Msg {
    pub id: String,
    pub role: String,
    pub text: String,
    pub ts: Option<i64>,
    pub tool: Option<ToolRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    /// a skill/command the user invoked with this message: {name, path?, kind}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<Value>,
    /// "ask-answer": the user's answer to an ask (`answers`, `answerChannel` dashboard|terminal)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<crate::ask::AnswerItem>>,
    #[serde(rename = "answerChannel", skip_serializing_if = "Option::is_none")]
    pub answer_channel: Option<&'static str>,
    /// role "error": the provider error that stopped the turn {status, message, provider, model, at}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::session::ErrInfo>,
    /// role "shell": a "!cmd" / "$ code" the user ran in omp
    /// {kind: bash|python, command, output (first 4000 chars), outputLength, exitCode, cancelled, truncated}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<Value>,
    /// kind "btw": the question/answer pair id
    #[serde(rename = "btwId", skip_serializing_if = "Option::is_none")]
    pub btw_id: Option<String>,
    /// btw question: pending | answered | error; btw answer: "streaming" while omp writes it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    /// role "media": images a tool returned, `[{i, mime}]`, loaded through
    /// `/api/agents/{name}/attachment?msg=<id>&i=<i>` (the bytes stay in the session file)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Value>>,
}

/// Where a tool-result image lives: an omp blob (`~/.omp/agent/blobs/<sha256>`) or inline base64.
#[derive(Debug, PartialEq)]
pub enum ImgData {
    Blob(String),
    Base64(String),
}

/// Images in one session record's tool results, in order: (mime, data). Known formats only:
/// omp `toolResult` content `{type:"image", mimeType, data}` (data `blob:sha256:<hex>` or
/// base64), Claude Code `tool_result` content `{type:"image", source:{type:"base64",
/// media_type, data}}`, Codex `function_call_output` output `{type:"input_image",
/// image_url:"data:<mime>;base64,…"}`.
pub fn image_blocks(kind: &str, r: &Value) -> Vec<(String, ImgData)> {
    let mut out = vec![];
    let mut push = |mime: Option<&str>, data: Option<&str>| {
        if let (Some(m), Some(d)) = (mime, data) {
            if !m.starts_with("image/") {
                return;
            }
            let d = match d.strip_prefix("blob:sha256:") {
                Some(h) if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) => ImgData::Blob(h.to_ascii_lowercase()),
                Some(_) => return,
                None => ImgData::Base64(d.to_string()),
            };
            out.push((m.to_string(), d));
        }
    };
    match kind {
        "omp" if r["type"] == "message" && r["message"]["role"] == "toolResult" => {
            for c in r["message"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "image") {
                push(c["mimeType"].as_str(), c["data"].as_str());
            }
        }
        "claude" if r["type"] == "user" => {
            for tr in r["message"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "tool_result") {
                for c in tr["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "image" && c["source"]["type"] == "base64") {
                    push(c["source"]["media_type"].as_str(), c["source"]["data"].as_str());
                }
            }
        }
        "codex" if r["type"] == "response_item" && r["payload"]["type"] == "function_call_output" => {
            for c in r["payload"]["output"].as_array().into_iter().flatten().filter(|c| c["type"] == "input_image") {
                let url = c["image_url"].as_str().unwrap_or("");
                if let Some((head, data)) = url.strip_prefix("data:").and_then(|u| u.split_once(";base64,")) {
                    push(Some(head), Some(data));
                }
            }
        }
        _ => {}
    }
    out
}

/// Bytes of image `i` of the record at byte `off` of a session file (≤ 20 MB, raster types).
pub fn image_at(kind: &str, path: &Path, off: u64, i: usize) -> Res<(String, Vec<u8>)> {
    use std::io::{BufRead, Seek};
    let mut f = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
    f.seek(std::io::SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    std::io::Read::take(&mut f, 256 << 20).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
    let r: Value = serde_json::from_slice(&line).map_err(|_| "no record at that place (the session file changed)".to_string())?;
    let (mime, data) = image_blocks(kind, &r).into_iter().nth(i).ok_or("no such image")?;
    if !["image/png", "image/jpeg", "image/webp", "image/gif"].contains(&mime.as_str()) {
        return Err(format!("unsupported image type {mime}"));
    }
    let bytes = match data {
        ImgData::Blob(h) => {
            let dir = crate::util::home().join(".omp/agent/blobs");
            let ext = mime.trim_start_matches("image/").replace("jpeg", "jpg");
            [dir.join(format!("{h}.{ext}")), dir.join(&h)].iter().find_map(|p| std::fs::read(p).ok()).ok_or("image blob is gone")?
        }
        ImgData::Base64(s) => crate::util::base64_decode(&s).ok_or("bad base64")?,
    };
    if bytes.len() > 20 << 20 {
        return Err("image larger than 20 MB".into());
    }
    Ok((mime, bytes))
}

#[derive(Default)]
struct TFile {
    pos: crate::util::JsonlPos,
    /// agy: step idx the next read starts after
    step: u64,
    msgs: Vec<Msg>,
    /// last use (ms), for pruning
    used: i64,
    /// btw-history signature last merged (omp)
    btw_sig: String,
}

/// Merge omp btw entries into the parsed messages: the question after the record it was asked
/// at (`leafId`), else by time; the answer right after its question.
fn merge_btw(msgs: &mut Vec<Msg>, entries: &[crate::btw::Entry]) {
    msgs.retain(|m| m.kind != Some("btw"));
    for e in entries {
        let mut q = msg(format!("btw:{}:q", e.id), "user", e.question.clone(), Some(e.created), None);
        q.kind = Some("btw");
        q.btw_id = Some(e.id.clone());
        q.status = Some(e.q_status());
        if q.status == Some("error") {
            q.error = Some(crate::session::ErrInfo { message: e.error.clone().unwrap_or_else(|| "btw failed".into()), at: e.updated, ..Default::default() });
        }
        let after_leaf = e.leaf.as_deref().and_then(|l| msgs.iter().rposition(|m| m.id == l || m.id.starts_with(&format!("{l}."))));
        let at = after_leaf.map(|i| {
            // past the leaf's own tool lines and earlier btw pairs at the same place
            let mut j = i + 1;
            while j < msgs.len() && (msgs[j].id.starts_with(&format!("{}.", msgs[i].id)) || msgs[j].kind == Some("btw")) {
                j += 1;
            }
            j
        });
        let at = at.unwrap_or_else(|| msgs.iter().position(|m| m.ts.is_some_and(|t| t > e.created)).unwrap_or(msgs.len()));
        let mut at = at;
        msgs.insert(at, q);
        if !e.answer.trim().is_empty() {
            let mut a = msg(format!("btw:{}:a", e.id), "assistant", e.answer.clone(), Some(e.updated), None);
            a.kind = Some("btw");
            a.btw_id = Some(e.id.clone());
            a.status = (e.q_status() == "pending").then_some("streaming");
            at += 1;
            msgs.insert(at, a);
        }
        // follow-ups asked in the btw panel: their own pairs (btwId "<id>.fN") right after
        for f in &e.follow_ups {
            let mut q = msg(format!("btw:{}:q", f.id), "user", f.question.clone(), Some(f.created), None);
            q.kind = Some("btw");
            q.btw_id = Some(f.id.clone());
            q.status = Some(f.q_status());
            at += 1;
            msgs.insert(at, q);
            if !f.answer.trim().is_empty() {
                let mut a = msg(format!("btw:{}:a", f.id), "assistant", f.answer.clone(), Some(f.updated), None);
                a.kind = Some("btw");
                a.btw_id = Some(f.id.clone());
                a.status = (f.q_status() == "pending").then_some("streaming");
                at += 1;
                msgs.insert(at, a);
            }
        }
    }
}

#[derive(Default)]
pub struct TranscriptCache {
    files: HashMap<PathBuf, TFile>,
}

fn ts_of(v: &Value) -> Option<i64> {
    v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis())
}

/// "name {short args}" ≤160 chars, args compacted to one line.
fn summary(name: &str, args: &Value) -> String {
    let a = match args {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Object(o) => {
            // prefer a human-ish field first
            let key = ["command", "cmd", "path", "file_path", "pattern", "query", "url", "i", "description"]
                .iter()
                .find(|k| o.get(**k).is_some_and(|v| v.is_string()));
            match key {
                Some(k) => o[*k].as_str().unwrap_or("").to_string(),
                None => args.to_string(),
            }
        }
        other => other.to_string(),
    };
    let one = a.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(format!("{name} {one}").trim(), 159)
}

fn text_parts(content: &Value, kinds: &[&str]) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            a.iter().filter(|c| c["type"].as_str().is_some_and(|t| kinds.contains(&t))).filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

fn msg(id: String, role: &str, text: String, ts: Option<i64>, tool: Option<ToolRef>) -> Msg {
    let attachments = if role == "user" { attachments_of(&text) } else { None };
    Msg {
        id,
        role: role.into(),
        text,
        ts,
        tool,
        attachments,
        skill: None,
        kind: None,
        answers: None,
        answer_channel: None,
        error: None,
        shell: None,
        btw_id: None,
        status: None,
        images: None,
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Attachment {
    pub path: String,
    pub kind: &'static str,
}

static ATTACH_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[첨부 이미지 \d+\] (\S+)|(/\S*?/adash/uploads/\S+?\.(?:png|jpg|webp|gif))").unwrap());

/// Image paths a user message refers to: adash `[첨부 이미지 k]` lines and any adash upload path.
pub fn attachments_of(text: &str) -> Option<Vec<Attachment>> {
    let mut out: Vec<Attachment> = Vec::new();
    for c in ATTACH_RE.captures_iter(text) {
        let p = c.get(1).or(c.get(2)).map(|m| m.as_str().trim_end_matches(['.', ',', ')', '`', '"', '\'']).to_string()).unwrap_or_default();
        if !p.is_empty() && !out.iter().any(|a| a.path == p) {
            out.push(Attachment { path: p, kind: "image" });
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A user turn in an omp record: plain user messages, and skill invocations, which omp records
/// as `custom_message` / `skill-prompt` (attribution user) with the typed text in
/// `details.prompt` and the skill body in `content` (not the user's words).
pub fn omp_user(r: &Value) -> Option<(String, Option<Value>)> {
    match r["type"].as_str()? {
        "message" if r["message"]["role"] == "user" => Some((text_parts(&r["message"]["content"], &["text"]), None)),
        "custom_message" if r["customType"] == "skill-prompt" && r["attribution"] == "user" => {
            let d = &r["details"];
            let name = d["name"].as_str().unwrap_or("");
            let text =
                d["prompt"].as_str().map(String::from).unwrap_or_else(|| format!("/skill:{name} {}", d["args"].as_str().unwrap_or("")).trim().to_string());
            Some((text, Some(json!({"name": name, "path": d["path"], "kind": "skill"}))))
        }
        _ => None,
    }
}

/// Claude Code's slash-command record (`<command-name>/x</command-name>…<command-args>…`):
/// the typed form `/x args` and the command as skill metadata.
pub fn claude_command(text: &str) -> Option<(String, Value)> {
    let t = text.trim_start();
    if !t.starts_with("<command-name>") && !t.starts_with("<command-message>") {
        return None;
    }
    let tag = |n: &str| t.split_once(&format!("<{n}>")).and_then(|(_, r)| r.split_once(&format!("</{n}>"))).map(|(v, _)| v.trim().to_string());
    let name = tag("command-name")?;
    let args = tag("command-args").unwrap_or_default();
    let typed = if args.is_empty() { name.clone() } else { format!("{name} {args}") };
    Some((typed, json!({"name": name.trim_start_matches('/'), "kind": "command"})))
}

/// The user's answer to an ask, as a user message (question → chosen answers).
fn ask_answer(id: String, ts: Option<i64>, call_id: &str, v: &Value) -> Option<Msg> {
    let items = crate::ask::answer_items(v);
    if items.is_empty() {
        return None;
    }
    let channel = if crate::ask::DASHBOARD_ANSWERS.plock().contains(call_id) { "dashboard" } else { "terminal" };
    let mut m = msg(id, "user", crate::ask::answer_text(&items), ts, None);
    m.kind = Some("ask-answer");
    m.answers = Some(items);
    m.answer_channel = Some(channel);
    Some(m)
}

/// A provider error as a transcript message (role "error").
fn error_msg(id: String, e: crate::session::ErrInfo) -> Msg {
    let text = format!("오류{} · {}", e.status.map(|s| format!(" {s}")).unwrap_or_default(), e.message);
    let mut m = msg(id, "error", text, Some(e.at), None);
    m.error = Some(e);
    m
}

pub(crate) fn parse_omp(r: &Value, out: &mut Vec<Msg>) {
    let id = r["id"].as_str().unwrap_or("").to_string();
    let ts = ts_of(&r["timestamp"]);
    if r["type"] == "custom_message" {
        match r["customType"].as_str() {
            Some("skill-prompt") => {
                if let Some((text, skill)) = omp_user(r) {
                    let mut m = msg(id, "user", text, ts, None);
                    m.skill = skill;
                    out.push(m);
                } else if r["display"] == true {
                    // a skill prompt the agent itself injected
                    out.push(msg(id, "system", format!("스킬 {} 불러옴", r["details"]["name"].as_str().unwrap_or("")), ts, None));
                }
            }
            // another agent's message over omp IRC
            Some("irc:incoming") if r["display"] == true => {
                let d = &r["details"];
                let text = d["message"].as_str().map(String::from).unwrap_or_else(|| text_parts(&r["content"], &["text"]));
                if !text.trim().is_empty() {
                    out.push(msg(id, "system", format!("[IRC {}] {}", d["from"].as_str().unwrap_or("?"), text), ts, None));
                }
            }
            // an image pasted with the previous user message
            Some("image-attachment") if r["attribution"] == "user" => {
                if let (Some(p), Some(last)) = (r["details"]["path"].as_str(), out.iter_mut().rev().find(|m| m.role == "user")) {
                    let a = last.attachments.get_or_insert_with(Vec::new);
                    if !a.iter().any(|x| x.path == p) {
                        a.push(Attachment { path: p.to_string(), kind: "image" });
                    }
                }
            }
            _ => {}
        }
        return;
    }
    if r["type"] != "message" {
        return;
    }
    let m = &r["message"];
    match m["role"].as_str().unwrap_or("") {
        "user" => out.push(msg(id, "user", text_parts(&m["content"], &["text"]), ts, None)),
        "bashExecution" | "pythonExecution" => {
            let kind = if m["role"] == "bashExecution" { "bash" } else { "python" };
            let command = m["command"].as_str().or(m["code"].as_str()).unwrap_or("").to_string();
            let output = m["output"].as_str().unwrap_or("");
            let mut x = msg(id, "shell", command.clone(), ts, None);
            x.shell = Some(json!({
                "kind": kind, "command": command, "output": output.chars().take(4000).collect::<String>(),
                "outputLength": output.chars().count(), "exitCode": m["exitCode"], "cancelled": m["cancelled"].as_bool().unwrap_or(false),
                "truncated": m["truncated"].as_bool().unwrap_or(false),
            }));
            out.push(x);
        }
        "developer" | "system" => out.push(msg(id, "system", text_parts(&m["content"], &["text"]), ts, None)),
        "assistant" => {
            let text = text_parts(&m["content"], &["text"]);
            if !text.trim().is_empty() {
                out.push(msg(id.clone(), "assistant", text, ts, None));
            }
            for (i, c) in m["content"].as_array().into_iter().flatten().enumerate() {
                if c["type"] == "toolCall" {
                    let name = c["name"].as_str().unwrap_or("tool");
                    let s = summary(name, &c["arguments"]);
                    out.push(msg(format!("{id}.{i}"), "tool", s.clone(), ts, Some(tool_ref(name, s, &c["arguments"]))));
                }
            }
            if let Some(e) = crate::session::record_error("omp", r, ts.unwrap_or(0)) {
                out.push(error_msg(format!("{id}.err"), e));
            }
        }
        "toolResult" if m["toolName"] == "ask" && m["isError"] != true => {
            if let Some(a) = ask_answer(id, ts, m["toolCallId"].as_str().unwrap_or(""), &m["details"]) {
                out.push(a);
            }
        }
        "toolResult" if m["isError"] == true => {
            let name = m["toolName"].as_str().unwrap_or("tool");
            let t = truncate(text_parts(&m["content"], &["text"]).trim(), 300);
            out.push(msg(id, "tool", format!("오류: {t}"), ts, Some(ToolRef { name: name.into(), summary: format!("{name} error"), paths: vec![] })));
        }
        _ => {}
    }
}

pub(crate) fn parse_claude(r: &Value, out: &mut Vec<Msg>) {
    let role = r["type"].as_str().unwrap_or("");
    if role != "user" && role != "assistant" {
        return;
    }
    let m = &r["message"];
    let id = r["uuid"].as_str().unwrap_or("").to_string();
    let ts = ts_of(&r["timestamp"]);
    let content = &m["content"];
    if let Some(e) = crate::session::record_error("claude", r, ts.unwrap_or(0)) {
        out.push(error_msg(id, e));
        return;
    }
    let text = text_parts(content, &["text"]);
    if !text.trim().is_empty() && r["isMeta"] != true {
        match (role, claude_command(&text)) {
            ("user", Some((typed, skill))) => {
                let mut m = msg(id.clone(), role, typed, ts, None);
                m.skill = Some(skill);
                out.push(m);
            }
            _ => out.push(msg(id.clone(), role, text, ts, None)),
        }
    }
    for (i, c) in content.as_array().into_iter().flatten().enumerate() {
        match c["type"].as_str() {
            Some("tool_use") => {
                let name = c["name"].as_str().unwrap_or("tool");
                let s = summary(name, &c["input"]);
                out.push(msg(format!("{id}.{i}"), "tool", s.clone(), ts, Some(tool_ref(name, s, &c["input"]))));
            }
            Some("tool_result") if c["is_error"] != true && ask_answer(String::new(), None, "", &c["content"]).is_some() => {
                if let Some(a) = ask_answer(format!("{id}.{i}"), ts, c["tool_use_id"].as_str().unwrap_or(""), &c["content"]) {
                    out.push(a);
                }
            }
            Some("tool_result") if c["is_error"] == true => {
                let t = truncate(text_parts(&c["content"], &["text"]).trim(), 300);
                out.push(msg(
                    format!("{id}.{i}"),
                    "tool",
                    format!("오류: {t}"),
                    ts,
                    Some(ToolRef { name: "tool".into(), summary: "tool error".into(), paths: vec![] }),
                ));
            }
            _ => {}
        }
    }
}

pub(crate) fn parse_codex(r: &Value, n: usize, out: &mut Vec<Msg>) {
    if let Some(e) = crate::session::record_error("codex", r, ts_of(&r["timestamp"]).unwrap_or(0)) {
        out.push(error_msg(format!("c{n}.err"), e));
        return;
    }
    if r["type"] != "response_item" {
        return;
    }
    let p = &r["payload"];
    let ts = ts_of(&r["timestamp"]);
    let id = p["id"].as_str().map(String::from).unwrap_or_else(|| format!("c{n}"));
    match p["type"].as_str() {
        Some("message") => {
            let text = text_parts(&p["content"], &["input_text", "output_text", "text"]);
            if text.trim().is_empty() {
                return;
            }
            let role = match p["role"].as_str() {
                Some("assistant") => "assistant",
                Some("user") if !text.trim_start().starts_with('<') => "user",
                _ => "system",
            };
            // `$skill` in a codex prompt injects `<skill><name>…</name><path>…</path>…`: the
            // user's message gets the skill as metadata; the body is not shown
            if text.trim_start().starts_with("<skill>") {
                let tag = |n: &str| text.split_once(&format!("<{n}>")).and_then(|(_, r)| r.split_once(&format!("</{n}>"))).map(|(v, _)| v.trim().to_string());
                if let (Some(name), Some(last)) = (tag("name"), out.iter_mut().rev().find(|m| m.role == "user")) {
                    last.skill.get_or_insert_with(|| json!({"name": name, "path": tag("path"), "kind": "skill"}));
                }
                return;
            }
            out.push(msg(id, role, text, ts, None));
        }
        Some("function_call") => {
            let name = p["name"].as_str().unwrap_or("tool");
            let args = serde_json::from_str::<Value>(p["arguments"].as_str().unwrap_or("")).unwrap_or(Value::Null);
            let s = summary(name, &args);
            out.push(msg(id, "tool", s.clone(), ts, Some(tool_ref(name, s, &args))));
        }
        Some("custom_tool_call") => {
            let name = p["name"].as_str().unwrap_or("tool");
            let s = summary(name, &p["input"]);
            out.push(msg(id, "tool", s.clone(), ts, Some(tool_ref(name, s, &p["input"]))));
        }
        _ => {}
    }
}

fn source_of(agent_type: &str) -> &'static str {
    crate::harness::get(agent_type).map(|h| h.transcript_source()).unwrap_or("terminal")
}

impl TranscriptCache {
    /// Keep parsed transcripts of live agents and the 24 most recently used others.
    pub fn prune(&mut self, live: &std::collections::HashSet<PathBuf>) {
        let mut others: Vec<(i64, PathBuf)> = self.files.iter().filter(|(p, _)| !live.contains(*p)).map(|(p, t)| (t.used, p.clone())).collect();
        others.sort_by_key(|x| std::cmp::Reverse(x.0));
        for (_, p) in others.into_iter().skip(24) {
            self.files.remove(&p);
        }
    }

    /// Parse only bytes appended since the last call.
    fn update(&mut self, kind: &str, path: &Path) -> &Vec<Msg> {
        let tf = self.files.entry(path.to_path_buf()).or_default();
        tf.used = crate::util::now_ms();
        if tf.pos.moved(path) {
            *tf = TFile::default();
            tf.used = crate::util::now_ms();
        }
        let before = tf.pos.offset;
        let msgs = &mut tf.msgs;
        tf.pos.read(path, |off, line| {
            let Ok(r) = serde_json::from_slice::<Value>(line) else { return };
            let n = msgs.len();
            if let Some(h) = crate::harness::get(kind) {
                h.parse_record(&r, n, msgs);
            }
            // images a tool returned: shown in the chat (the agent showed them to the user)
            let imgs = image_blocks(kind, &r);
            if !imgs.is_empty() {
                let mut m = msg(format!("img{off}"), "media", String::new(), ts_of(&r["timestamp"]), None);
                m.images = Some(imgs.iter().enumerate().map(|(i, (mime, _))| json!({"i": i, "mime": mime})).collect());
                msgs.push(m);
            }
        });
        if tf.pos.offset != before {
            // re-place btw entries after new records
            tf.btw_sig.clear();
        }
        if kind == "omp" {
            let sig = crate::btw::sig(path);
            if sig != tf.btw_sig {
                merge_btw(&mut tf.msgs, &crate::btw::load(path));
                tf.btw_sig = sig;
            }
        }
        &tf.msgs
    }

    pub fn transcript(&mut self, a: &Agent, after: Option<&str>, limit: usize, want: Option<&str>, around: Option<&str>) -> Res<Value> {
        let limit = limit.clamp(1, 2000);
        let src = if want == Some("terminal") { "terminal" } else { source_of(&a.agent_type) };
        if let (Some(path), "antigravity-db") = (&a.session_path, src) {
            if let Some(v) = self.agy_transcript(a, path, after, limit, around) {
                return Ok(v);
            }
        }
        // a large session file that yields no message is a format adash does not know:
        // say so once and show the terminal
        let unreadable =
            |c: &mut Self, path: &Path| c.update(&a.agent_type, path).is_empty() && std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > 64 * 1024;
        if let (Some(path), "omp-jsonl" | "claude-jsonl" | "codex-rollout") = (&a.session_path, src) {
            if unreadable(self, path) {
                crate::harness::degrade(&a.agent_type, "no messages recognized in the session file");
                return self.transcript(a, after, limit, Some("terminal"), around);
            }
            // the tag carries the btw signature: a btw entry placed mid-list changes it, and the
            // client reloads (replace) instead of appending by count
            let sig = self.files.get(path.as_path()).map(|t| t.btw_sig.clone()).unwrap_or_default();
            let msgs = &self.files[path.as_path()].msgs;
            let tag: String =
                path.file_stem().map(|s| s.to_string_lossy().chars().rev().take(8).collect::<String>()).unwrap_or_default() + &sig.replace('.', "x");
            // cursor "m<count>:<file tag>"; a cursor from another session file means start over
            let parsed =
                after.and_then(|c| c.strip_prefix('m')).and_then(|c| c.split_once(':')).filter(|(_, t)| *t == tag).and_then(|(n, _)| n.parse::<usize>().ok());
            let replace = after.is_some() && parsed.is_none();
            let (start, end) = window(msgs, parsed, around, limit);
            return Ok(json!({
                "agent": a.name, "source": src,
                "mappingConfidence": a.mapping_confidence,
                "cursor": format!("m{end}:{tag}"),
                "messages": &msgs[start..end],
                "replace": replace,
                "state": a.state, "tool": a.tool,
            }));
        }
        // an agent process whose session file does not exist yet (nothing sent so far): an empty
        // conversation instead of the terminal screen; `source=terminal` still shows the screen
        if want != Some("terminal") && a.session_path.is_none() && a.handle.is_some() && crate::harness::get(&a.agent_type).is_some() {
            let launch = a.handle.as_deref().and_then(|h| crate::launch::LAUNCH_PROMPTS.lock().unwrap_or_else(|e| e.into_inner()).get(h).cloned());
            let messages: Vec<Msg> = launch
                .map(|(p, ts)| {
                    let mut m = msg("pending:launch".into(), "user", p, Some(ts), None);
                    m.status = Some("pending");
                    m
                })
                .into_iter()
                .collect();
            let cursor = format!("pending:{}", messages.len());
            let unchanged = after == Some(cursor.as_str());
            return Ok(json!({
                "agent": a.name, "source": "pending",
                "mappingConfidence": a.mapping_confidence,
                "hint": "첫 메시지를 보내면 대화가 여기에 표시됩니다",
                "terminalAvailable": true,
                "cursor": cursor, "messages": if unchanged { vec![] } else { messages }, "replace": true,
                "state": a.state, "tool": a.tool,
            }));
        }
        // terminal fallback: one system message per read, replaced (not appended)
        let h = a.handle.as_deref().ok_or("agent has no terminal")?;
        let (_, lines) = term::tail_n(h, 400)?;
        let cursor = {
            use std::hash::{Hash, Hasher};
            let mut hs = std::collections::hash_map::DefaultHasher::new();
            lines.hash(&mut hs);
            format!("{:016x}", hs.finish())
        };
        let unchanged = after == Some(cursor.as_str());
        let messages = if unchanged { vec![] } else { vec![msg(format!("terminal:{cursor}"), "system", lines.join("\n").trim_end().to_string(), None, None)] };
        Ok(json!({
            "agent": a.name, "source": "terminal",
            "mappingConfidence": "exact",
            "cursor": cursor, "messages": messages, "replace": true,
            "state": a.state, "tool": a.tool,
        }))
    }
}

impl TranscriptCache {
    /// Path-like tool arguments from the agent's most recent `max_calls` tool calls, newest first.
    pub fn recent_tool_paths(&mut self, a: &Agent, max_calls: usize) -> Vec<String> {
        let Some(path) = &a.session_path else { return vec![] };
        if !matches!(a.agent_type.as_str(), "omp" | "claude" | "codex") {
            return vec![];
        }
        self.update(&a.agent_type, path).iter().rev().filter_map(|m| m.tool.as_ref()).take(max_calls).flat_map(|t| t.paths.iter().cloned()).collect()
    }
}

impl TranscriptCache {
    /// agy conversation db → messages (user/assistant/tool), incremental by step idx.
    /// None when the db can't be read or decoded (caller falls back to the terminal).
    fn agy_transcript(&mut self, a: &Agent, db: &Path, after: Option<&str>, limit: usize, around: Option<&str>) -> Option<Value> {
        let msgs = self.agy_messages(db)?;
        let tag: String = db.file_stem().map(|s| s.to_string_lossy().chars().take(8).collect()).unwrap_or_default();
        let parsed =
            after.and_then(|c| c.strip_prefix('m')).and_then(|c| c.split_once(':')).filter(|(_, t)| *t == tag).and_then(|(n, _)| n.parse::<usize>().ok());
        let (start, end) = window(&msgs, parsed, around, limit);
        Some(json!({
            "agent": a.name, "source": "antigravity-db", "mappingConfidence": "exact",
            "cursor": format!("m{end}:{tag}"), "messages": &msgs[start..end], "state": a.state, "tool": a.tool,
        }))
    }

    fn agy_messages(&mut self, db: &Path) -> Option<Vec<Msg>> {
        let tf = self.files.entry(db.to_path_buf()).or_default();
        tf.used = crate::util::now_ms();
        let steps = crate::agy::read_steps(db, tf.step as i64).ok()?;
        // a running step is read again until it finishes (agy updates the row in place)
        tf.step = crate::agy::resume_after(tf.step as i64, &steps).max(0) as u64;
        for s in steps {
            let id = format!("agy{}", s.idx);
            let ts = Some(s.ts).filter(|t| *t > 0);
            let m = match s.kind {
                crate::agy::USER => Some(msg(id.clone(), "user", s.text.unwrap_or_default(), ts, None)),
                crate::agy::MODEL => {
                    // thinking first (folded in the chat), then the answer text
                    if let Some(t) = s.thinking.clone() {
                        let tid = format!("{id}.think");
                        let m = msg(tid.clone(), "thinking", t, ts, None);
                        match tf.msgs.iter().rposition(|x| x.id == tid) {
                            Some(i) => tf.msgs[i] = m,
                            None => tf.msgs.push(m),
                        }
                    }
                    s.text.filter(|t| !t.trim().is_empty()).map(|t| msg(id.clone(), "assistant", t, ts, None))
                }
                crate::agy::SYSTEM => s.text.filter(|t| !t.trim().is_empty()).map(|t| msg(id.clone(), "system", t, ts, None)),
                crate::agy::TOOL => s.tool.map(|(_, name, args)| {
                    let sm = summary(&name, &args);
                    msg(id.clone(), "tool", sm.clone(), ts, Some(tool_ref(&name, sm, &args)))
                }),
                _ => None,
            };
            let Some(m) = m else { continue };
            match tf.msgs.iter().rposition(|x| x.id == id) {
                Some(i) => tf.msgs[i] = m,
                None => tf.msgs.push(m),
            }
        }
        if tf.msgs.is_empty() {
            return None;
        }
        Some(tf.msgs.clone())
    }

    /// Read-only transcript of any session file by kind (offline sessions from search hits).
    pub fn session_view(&mut self, kind: &str, path: &Path, around: Option<&str>, limit: usize) -> Res<Value> {
        let limit = limit.clamp(1, 2000);
        let msgs: Vec<Msg> = if kind == "antigravity" { self.agy_messages(path).ok_or("conversation not decodable")? } else { self.update(kind, path).clone() };
        let (start, end) = window(&msgs, None, around, limit);
        Ok(
            json!({"source": source_of(kind), "file": path.to_string_lossy(), "total": msgs.len(), "start": start, "messages": &msgs[start..end], "readOnly": true}),
        )
    }
}

/// Message window: after a cursor, around a message id (centered), else the last `limit`.
fn window(msgs: &[Msg], after: Option<usize>, around: Option<&str>, limit: usize) -> (usize, usize) {
    let total = msgs.len();
    let start = match (around.and_then(|id| msgs.iter().position(|m| m.id == id || m.id.starts_with(&format!("{id}.")))), after) {
        (Some(pos), _) => pos.saturating_sub(limit / 2),
        (None, Some(n)) => n.min(total),
        (None, None) => total.saturating_sub(limit),
    };
    (start, (start + limit).min(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_answers_show_as_user_messages() {
        let mut out = vec![];
        let r = serde_json::json!({"type":"message","id":"t1","timestamp":"2026-10-30T07:00:00.000Z","message":{"role":"toolResult","toolCallId":"call_x","toolName":"ask","isError":false,
            "content":[{"type":"text","text":"User answers: …"}],
            "details":{"results":[{"id":"venues","question":"어느 학회?","multi":true,"selectedOptions":["NeurIPS","ICLR"]},{"id":"deadline","question":"마감은?","selectedOptions":[],"customInput":"내년 1월"}]}}});
        parse_omp(&r, &mut out);
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!((m.role.as_str(), m.kind, m.answer_channel), ("user", Some("ask-answer"), Some("terminal")));
        assert_eq!(m.text, "어느 학회? → NeurIPS, ICLR\n마감은? → 내년 1월");
        let a = m.answers.as_ref().unwrap();
        assert_eq!((a[0].picked.clone(), a[1].text.clone()), (vec!["NeurIPS".to_string(), "ICLR".to_string()], Some("내년 1월".to_string())));
        // dashboard-answered call ids are labelled so
        crate::ask::DASHBOARD_ANSWERS.plock().insert("call_x".into());
        let mut out2 = vec![];
        parse_omp(&r, &mut out2);
        assert_eq!(out2[0].answer_channel, Some("dashboard"));
        // Claude AskUserQuestion result text
        let items = crate::ask::answer_items(&serde_json::json!("User has answered your questions: \"그림 순서?\"=\"A안\". You can now continue."));
        assert_eq!((items[0].question.as_str(), items[0].picked.clone()), ("그림 순서?", vec!["A안".to_string()]));
    }

    #[test]
    fn skill_invocations_show_as_the_users_typed_message() {
        let mut out = vec![];
        let rec = serde_json::json!({"type":"custom_message","customType":"skill-prompt","display":true,"attribution":"user","id":"k1","timestamp":"2026-10-30T06:19:08.052Z","content":"[IMPORTANT: …] full skill body","details":{"name":"magi-research","path":"/s/magi-research/SKILL.md","args":"x","prompt":"음 약한 선도 좀 애매해. /skill:magi-research 에게 제안받자"}});
        parse_omp(&rec, &mut out);
        let paste = serde_json::json!({"type":"custom_message","customType":"image-attachment","display":false,"attribution":"user","id":"i1","details":{"index":1,"path":"/tmp/paste-1.png"}});
        parse_omp(&paste, &mut out);
        let irc = serde_json::json!({"type":"custom_message","customType":"irc:incoming","display":true,"attribution":"agent","id":"r1","details":{"from":"ScalarReuse","message":"ready for one parity run"}});
        parse_omp(&irc, &mut out);
        let noise = serde_json::json!({"type":"custom_message","customType":"async-result","display":true,"attribution":"agent","id":"n1"});
        parse_omp(&noise, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].role.as_str(), out[0].text.as_str()), ("user", "음 약한 선도 좀 애매해. /skill:magi-research 에게 제안받자"));
        assert_eq!(out[0].skill.as_ref().unwrap()["name"], "magi-research");
        assert_eq!(out[0].attachments.as_ref().unwrap()[0].path, "/tmp/paste-1.png");
        assert_eq!((out[1].role.as_str(), out[1].text.as_str()), ("system", "[IRC ScalarReuse] ready for one parity run"));
        // claude slash command tags → what was typed
        let (typed, sk) =
            claude_command("<command-name>/academic-cv</command-name>\n<command-message>academic-cv</command-message>\n<command-args>CV 고쳐줘</command-args>")
                .unwrap();
        assert_eq!((typed.as_str(), sk["name"].as_str()), ("/academic-cv CV 고쳐줘", Some("academic-cv")));
        assert!(claude_command("그냥 메시지").is_none());
        // codex `$skill`: the injected <skill> record becomes metadata of the user's message
        let mut c = vec![];
        parse_codex(
            &serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"$academic-cv 봐 줘"}]}}),
            0,
            &mut c,
        );
        parse_codex(
            &serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<skill>\n<name>academic-cv</name>\n<path>/s/academic-cv/SKILL.md</path>\nbody</skill>"}]}}),
            1,
            &mut c,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].skill.as_ref().unwrap()["path"], "/s/academic-cv/SKILL.md");
    }
    use super::attachments_of;

    #[test]
    fn user_message_image_paths_become_attachments() {
        let t = "이 그래프 봐 주세요\n[첨부 이미지 1] /home/u/.local/share/adash/uploads/20261029/101500-ab12cd34ef.png — 읽기 도구로 이 이미지를 열어 확인하세요.\n그리고 /tmp/screenshot-1793176460135.png 도.";
        let a: Vec<String> = attachments_of(t).unwrap().into_iter().map(|a| a.path).collect();
        assert_eq!(a, vec!["/home/u/.local/share/adash/uploads/20261029/101500-ab12cd34ef.png"]);
        assert!(attachments_of("plain text /tmp/other.png").is_none());
    }

    #[test]
    fn tool_result_images_become_a_media_message_loaded_on_demand() {
        let png = [0x89u8, b'P', b'N', b'G', 1, 2, 3];
        let b64 = "iVBORwECAw=="; // the bytes above
        assert_eq!(crate::util::base64_decode(b64).unwrap(), png);
        let omp = json!({"type":"message","id":"r1","timestamp":"2026-11-04T05:41:34.295Z","message":{"role":"toolResult","toolName":"eval","content":[
            {"type":"text","text":"shown"},
            {"type":"image","mimeType":"image/png","data":format!("blob:sha256:{}", "a".repeat(64))},
            {"type":"image","mimeType":"image/png","data":b64}]}});
        assert_eq!(image_blocks("omp", &omp), vec![("image/png".into(), ImgData::Blob("a".repeat(64))), ("image/png".into(), ImgData::Base64(b64.into()))]);
        let claude = json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"AAAA"}}]}]}});
        assert_eq!(image_blocks("claude", &claude), vec![("image/jpeg".into(), ImgData::Base64("AAAA".into()))]);
        let codex = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":[{"type":"input_image","image_url":"data:image/png;base64,AAAA"}]}});
        assert_eq!(image_blocks("codex", &codex).len(), 1);
        // a user's own message or a non-image block: nothing
        assert!(image_blocks("omp", &json!({"type":"message","message":{"role":"user","content":[{"type":"image","mimeType":"image/png","data":"AAAA"}]}}))
            .is_empty());
        assert!(image_blocks(
            "omp",
            &json!({"type":"message","message":{"role":"toolResult","content":[{"type":"image","mimeType":"text/html","data":"AAAA"}]}})
        )
        .is_empty());

        // in a transcript: one media message with the image list, no base64 in the JSON; the
        // bytes come back from the record's offset
        let dir = std::env::temp_dir().join(format!("adash-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.jsonl");
        let first =
            json!({"type":"message","id":"a1","timestamp":"2026-11-04T05:41:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}})
                .to_string();
        let inline = json!({"type":"message","id":"r2","timestamp":"2026-11-04T05:41:34.295Z","message":{"role":"toolResult","toolName":"read","content":[{"type":"image","mimeType":"image/png","data":b64}]}}).to_string();
        std::fs::write(&f, format!("{first}\n{inline}\n")).unwrap();
        let mut c = TranscriptCache::default();
        let msgs = c.update("omp", &f).clone();
        let m = msgs.iter().find(|m| m.role == "media").expect("media message");
        assert_eq!(m.id, format!("img{}", first.len() + 1));
        assert_eq!(m.images.as_ref().unwrap()[0]["mime"], "image/png");
        assert!(!serde_json::to_string(&msgs).unwrap().contains(b64), "no base64 in the transcript");
        assert_eq!(image_at("omp", &f, first.len() as u64 + 1, 0).unwrap(), ("image/png".to_string(), png.to_vec()));
        assert!(image_at("omp", &f, 0, 0).is_err(), "an offset without an image record");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// agy's first step has idx 0 (the user's prompt); model steps carry their thinking at
    /// 20.3; step type 17 is a message agy adds itself (2026-11-09: a conversation showed tools only).
    #[test]
    fn agy_first_step_thinking_and_system_messages() {
        let dir = std::env::temp_dir().join(format!("adash-agyconv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("c.db");
        let ld = |field: u32, data: &[u8]| -> Vec<u8> {
            let tag = (field << 3) | 2;
            let mut o = if tag < 128 { vec![tag as u8] } else { vec![(tag as u8) | 0x80, (tag >> 7) as u8] };
            o.push(data.len() as u8);
            o.extend_from_slice(data);
            o
        };
        let user = ld(19, &ld(2, b"[v4] read the brief"));
        let model = ld(20, &ld(3, b"thinking it through"));
        let sys = ld(24, &ld(3, &ld(2, b"The stream was interrupted.")));
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER, status INTEGER, metadata BLOB, step_payload BLOB);").unwrap();
        for (i, k, p) in [(0, 14, &user), (1, 15, &model), (2, 17, &sys)] {
            c.execute("INSERT INTO steps VALUES (?1, ?2, 3, NULL, ?3)", rusqlite::params![i, k, p]).unwrap();
        }
        let mut tc = TranscriptCache::default();
        let m = tc.agy_messages(&db).unwrap();
        let roles: Vec<(&str, &str)> = m.iter().map(|x| (x.role.as_str(), x.text.as_str())).collect();
        assert_eq!(roles, vec![("user", "[v4] read the brief"), ("thinking", "thinking it through"), ("system", "The stream was interrupted.")]);
        assert_eq!(tc.agy_messages(&db).unwrap().len(), 3, "no duplicates on the next read");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
