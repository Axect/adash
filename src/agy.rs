//! Antigravity (`agy`) conversations: `~/.gemini/antigravity-cli/conversations/<id>.db`
//! (SQLite, steps as protobuf without a published schema). Decoded generically (wire format)
//! with field paths found by inspection (2026-10-29, agy CLI):
//! - step_type 14 = user input: text at `19.2`
//! - step_type 15 = model turn: markdown at `20.1`
//! - step_type 132 = tool call: `5.4.1` call id, `5.4.2` tool name, `5.4.3` JSON args
//! - metadata `1.1`/`1.2` = created (seconds/nanos); status 3 = finished
//!
//! The live process ↔ conversation link is the presence lock it holds:
//! `~/.gemini/antigravity-cli/presence/<conversationId>.lock`. Opened read-only, never written.
use crate::util::{home, Res};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    home().join(".gemini/antigravity-cli")
}

/// Conversation db of a running agy process (via its presence lock fd).
pub fn conversation_for_pid(pid: u32) -> Option<PathBuf> {
    crate::platform::open_files(pid).into_iter().find_map(|l| {
        let s = l.to_string_lossy();
        if s.contains("/antigravity-cli/presence/") && s.ends_with(".lock") {
            let id = l.file_stem()?.to_string_lossy().into_owned();
            let db = root().join("conversations").join(format!("{id}.db"));
            db.exists().then_some(db)
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------------------------
// protobuf wire format (schema-less)

#[derive(Debug, Clone)]
pub enum Field {
    Varint(u64),
    Bytes(Vec<u8>),
    Fixed,
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut r = 0u64;
    let mut shift = 0;
    loop {
        let x = *b.get(*i)?;
        *i += 1;
        r |= ((x & 0x7f) as u64) << shift;
        if x < 0x80 {
            return Some(r);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Top-level fields of a message; None when the bytes are not a valid message.
pub fn parse(b: &[u8]) -> Option<Vec<(u32, Field)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let key = varint(b, &mut i)?;
        let (f, w) = ((key >> 3) as u32, key & 7);
        if f == 0 {
            return None;
        }
        match w {
            0 => out.push((f, Field::Varint(varint(b, &mut i)?))),
            1 => {
                i += 8;
                out.push((f, Field::Fixed));
            }
            5 => {
                i += 4;
                out.push((f, Field::Fixed));
            }
            2 => {
                let n = varint(b, &mut i)? as usize;
                let end = i.checked_add(n)?;
                out.push((f, Field::Bytes(b.get(i..end)?.to_vec())));
                i = end;
            }
            _ => return None,
        }
    }
    (i == b.len()).then_some(out)
}

/// Bytes at a field path (first match at each level), e.g. `[19, 2]`.
pub fn bytes_at(b: &[u8], path: &[u32]) -> Option<Vec<u8>> {
    let (head, rest) = path.split_first()?;
    for (f, v) in parse(b)? {
        if f == *head {
            if let Field::Bytes(x) = v {
                if rest.is_empty() {
                    return Some(x);
                }
                if let Some(r) = bytes_at(&x, rest) {
                    return Some(r);
                }
            }
        }
    }
    None
}

pub fn str_at(b: &[u8], path: &[u32]) -> Option<String> {
    bytes_at(b, path).and_then(|x| String::from_utf8(x).ok())
}

pub fn varint_at(b: &[u8], path: &[u32]) -> Option<u64> {
    let (last, parents) = path.split_last()?;
    let msg = if parents.is_empty() { b.to_vec() } else { bytes_at(b, parents)? };
    parse(&msg)?.into_iter().find_map(|(f, v)| match v {
        Field::Varint(x) if f == *last => Some(x),
        _ => None,
    })
}

// ---------------------------------------------------------------------------------------------
// steps

#[derive(Debug, Clone)]
pub struct Step {
    pub idx: i64,
    pub kind: i64,
    pub status: i64,
    pub ts: i64,
    pub text: Option<String>,
    /// (call id, tool name, args)
    pub tool: Option<(String, String, Value)>,
}

pub const USER: i64 = 14;
pub const MODEL: i64 = 15;
pub const TOOL: i64 = 132;
pub const FINISHED: i64 = 3;

pub fn open(db: &Path) -> Res<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| format!("{}: {e}", db.display()))
}

/// Steps with idx > `after` in order.
/// The model a conversation ran on: the most frequent `gemini-*` name in its generator metadata.
pub fn model_of(db: &Path) -> Option<String> {
    let c = open(db).ok()?;
    let mut st = c.prepare("SELECT data FROM gen_metadata").ok()?;
    let re = regex::Regex::new(r"gemini-[0-9][a-z0-9.\-]*[a-z0-9]").ok()?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let rows = st.query_map([], |r| r.get::<_, Option<Vec<u8>>>(0)).ok()?;
    for b in rows.flatten().flatten() {
        let s = String::from_utf8_lossy(&b);
        for m in re.find_iter(&s) {
            *counts.entry(m.as_str().to_string()).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|x| (x.1, x.0.len())).map(|x| x.0)
}

pub fn read_steps(db: &Path, after: i64) -> Res<Vec<Step>> {
    let c = open(db)?;
    let mut st = c.prepare("SELECT idx, step_type, status, metadata, step_payload FROM steps WHERE idx > ?1 ORDER BY idx").map_err(|e| e.to_string())?;
    let rows = st
        .query_map([after], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<Vec<u8>>>(3)?, r.get::<_, Option<Vec<u8>>>(4)?))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        let (idx, kind, status, meta, payload) = r.map_err(|e| e.to_string())?;
        out.push(decode_step(idx, kind, status, meta.as_deref().unwrap_or(&[]), payload.as_deref().unwrap_or(&[])));
    }
    Ok(out)
}

pub fn decode_step(idx: i64, kind: i64, status: i64, meta: &[u8], payload: &[u8]) -> Step {
    let secs = varint_at(meta, &[1, 1]).or_else(|| varint_at(payload, &[5, 1, 1])).unwrap_or(0) as i64;
    let nanos = varint_at(meta, &[1, 2]).or_else(|| varint_at(payload, &[5, 1, 2])).unwrap_or(0) as i64;
    let ts = secs * 1000 + nanos / 1_000_000;
    let (text, tool) = match kind {
        USER => (str_at(payload, &[19, 2]).or_else(|| str_at(payload, &[19, 3, 1])), None),
        MODEL => (str_at(payload, &[20, 1]).or_else(|| str_at(payload, &[20, 8])), None),
        TOOL => {
            let id = str_at(payload, &[5, 4, 1]).unwrap_or_default();
            let name = str_at(payload, &[5, 4, 2]).unwrap_or_else(|| "tool".into());
            let args = str_at(payload, &[5, 4, 3]).and_then(|a| serde_json::from_str(&a).ok()).unwrap_or(Value::Null);
            (None, Some((id, name, args)))
        }
        _ => (None, None),
    };
    Step { idx, kind, status, ts, text, tool }
}

/// (agent mid-turn, since): working unless the last step is a finished model turn.
/// Where to read from next time: agy writes a model or tool step while it runs and later
/// updates the same row to FINISHED, so the first unfinished step (and everything after it)
/// is read again until it finishes. A step left unfinished far behind the end (abandoned) stops
/// holding the window open after 200 steps.
pub fn resume_after(prev: i64, steps: &[Step]) -> i64 {
    let last = steps.iter().map(|s| s.idx).max().unwrap_or(prev).max(prev);
    match steps.iter().find(|s| matches!(s.kind, MODEL | TOOL) && s.status != FINISHED) {
        Some(s) if last - s.idx < 200 => s.idx - 1,
        _ => last,
    }
}

pub fn phase(steps: &[Step]) -> Option<(bool, i64)> {
    let last = steps.last()?;
    let done = last.kind == MODEL && last.status == FINISHED;
    Some((!done, last.ts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn pb_bytes(field: u32, data: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        pb_varint(((field as u64) << 3) | 2, &mut o);
        pb_varint(data.len() as u64, &mut o);
        o.extend_from_slice(data);
        o
    }
    fn pb_v(field: u32, v: u64) -> Vec<u8> {
        let mut o = Vec::new();
        pb_varint((field as u64) << 3, &mut o);
        pb_varint(v, &mut o);
        o
    }

    /// Fixture built with the field layout observed in a real conversation db (2026-10-29).
    #[test]
    fn decodes_user_model_and_tool_steps() {
        let meta = pb_bytes(1, &[pb_v(1, 1793218496), pb_v(2, 906_398_013)].concat());
        let user = pb_bytes(19, &pb_bytes(2, "[survey trim review] @brief.md 를 읽고 답해줘".as_bytes()));
        let s = decode_step(1, USER, 3, &meta, &user);
        assert_eq!(s.text.as_deref(), Some("[survey trim review] @brief.md 를 읽고 답해줘"));
        assert_eq!(s.ts, 1793218496906);
        let model = pb_bytes(20, &[pb_bytes(1, "**결론:** 대안 1을 권합니다.".as_bytes()), pb_bytes(6, b"bot-6c98"), pb_v(12, 2)].concat());
        assert_eq!(decode_step(2, MODEL, 3, &meta, &model).text.as_deref(), Some("**결론:** 대안 1을 권합니다."));
        let call = pb_bytes(5, &pb_bytes(4, &[pb_bytes(1, b"call_6686740"), pb_bytes(2, b"edit_file"), pb_bytes(3, br#"{"Path":"/x/answer.md"}"#)].concat()));
        let t = decode_step(3, TOOL, 3, &meta, &call);
        let (id, name, args) = t.tool.unwrap();
        assert_eq!((id.as_str(), name.as_str(), args["Path"].as_str()), ("call_6686740", "edit_file", Some("/x/answer.md")));
        assert!(phase(&[decode_step(3, TOOL, 1, &meta, &call)]).unwrap().0);
        assert!(!phase(&[decode_step(2, MODEL, 3, &meta, &model)]).unwrap().0);
        assert!(parse(b"\xff\xff").is_none(), "garbage is not a message");
    }
}
