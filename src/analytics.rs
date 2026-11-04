//! Durable usage analytics: `~/.local/share/adash/analytics.db` (SQLite, WAL).
//! Append-only facts with natural keys (UNIQUE) so re-ingest/backfill never double counts.
//! Sources are read-only: omp/claude/codex session files, history.jsonl, thread files.
//! Schema reference: docs/analytics.md.
use crate::util::{now_ms, Res};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const SCHEMA_VERSION: i64 = 16;

pub fn data_dir() -> PathBuf {
    crate::platform::data_dir()
}

/// `ADASH_ANALYTICS_DB` points the CLI at a copy (dry runs of a migration/backfill).
pub fn db_path() -> PathBuf {
    std::env::var_os("ADASH_ANALYTICS_DB").map(PathBuf::from).unwrap_or_else(|| data_dir().join("analytics.db"))
}

const SCHEMA_V1: &str = r#"
CREATE TABLE ingest_files (
  path TEXT PRIMARY KEY, source TEXT NOT NULL, size INTEGER NOT NULL DEFAULT 0, mtime INTEGER NOT NULL DEFAULT 0,
  offset INTEGER NOT NULL DEFAULT 0, state TEXT, updated_at INTEGER
);
CREATE TABLE sessions (
  id INTEGER PRIMARY KEY, file TEXT NOT NULL UNIQUE, agent_type TEXT NOT NULL, session_uuid TEXT,
  parent_file TEXT, is_subagent INTEGER NOT NULL DEFAULT 0, origin TEXT, cwd TEXT, project TEXT, worktree TEXT,
  first_ts INTEGER, last_ts INTEGER, titles TEXT NOT NULL DEFAULT '[]', models TEXT NOT NULL DEFAULT '[]',
  handles TEXT NOT NULL DEFAULT '[]', names TEXT NOT NULL DEFAULT '[]', mapping TEXT
);
CREATE TABLE human_messages (
  id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER NOT NULL REFERENCES sessions(id),
  ts INTEGER NOT NULL, project TEXT, agent_name TEXT, agent_type TEXT, model TEXT,
  kind TEXT NOT NULL, channel TEXT NOT NULL, chars INTEGER, lines INTEGER, lang TEXT, attachments INTEGER,
  is_slash INTEGER, steering INTEGER, text TEXT, sha256 TEXT
);
CREATE INDEX human_messages_ts ON human_messages(ts);
CREATE TABLE agent_turns (
  id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER NOT NULL REFERENCES sessions(id),
  trigger_kind TEXT, start_ts INTEGER NOT NULL, end_ts INTEGER, duration_ms INTEGER, human_gap_ms INTEGER,
  project TEXT, agent_name TEXT, agent_type TEXT, model TEXT, assistant_chars INTEGER DEFAULT 0,
  tool_calls INTEGER DEFAULT 0, subagent_spawns INTEGER DEFAULT 0, input_tokens INTEGER DEFAULT 0,
  output_tokens INTEGER DEFAULT 0, cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
  cost REAL DEFAULT 0, interrupted INTEGER DEFAULT 0
);
CREATE INDEX agent_turns_start ON agent_turns(start_ts);
CREATE TABLE tool_calls (
  id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER NOT NULL REFERENCES sessions(id),
  turn_key TEXT, ts INTEGER NOT NULL, tool TEXT NOT NULL, duration_ms INTEGER, is_error INTEGER DEFAULT 0,
  project TEXT, agent_type TEXT, model TEXT
);
CREATE INDEX tool_calls_ts ON tool_calls(ts);
CREATE TABLE usage_events (
  id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER NOT NULL REFERENCES sessions(id),
  ts INTEGER NOT NULL, purpose TEXT, provider TEXT, model TEXT, project TEXT, agent_type TEXT,
  input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER, cost REAL
);
CREATE INDEX usage_events_ts ON usage_events(ts);
CREATE TABLE asks (
  id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER NOT NULL REFERENCES sessions(id),
  asked_at INTEGER NOT NULL, question TEXT, options TEXT, multi INTEGER, answered_at INTEGER, answer TEXT,
  cancelled INTEGER DEFAULT 0, channel TEXT
);
CREATE TABLE state_transitions (
  id INTEGER PRIMARY KEY, identity TEXT NOT NULL, ts INTEGER NOT NULL, state TEXT NOT NULL, prev_state TEXT,
  name TEXT, project TEXT, UNIQUE(identity, ts, state)
);
CREATE VIEW state_segments AS
  SELECT identity, name, project, state, ts AS start_ts,
         LEAD(ts) OVER (PARTITION BY identity ORDER BY ts) AS end_ts
  FROM state_transitions;
CREATE TABLE relay_threads (
  id TEXT PRIMARY KEY, from_agent TEXT, to_agent TEXT, created_at INTEGER, delivered_at INTEGER,
  answered_at INTEGER, relayed_at INTEGER, hops INTEGER, status TEXT
);
CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, kind TEXT, title TEXT, body TEXT, target TEXT, UNIQUE(ts, title));
CREATE TABLE dashboard_actions (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, action TEXT NOT NULL, target TEXT, detail TEXT, UNIQUE(ts, action, target));
CREATE TABLE snapshots (
  ts INTEGER PRIMARY KEY, live INTEGER, working INTEGER, waiting INTEGER, asking INTEGER, idle INTEGER, per_project TEXT
);
CREATE VIEW latencies AS
  SELECT session_id, rec_key, trigger_kind, start_ts, duration_ms AS agent_response_ms, human_gap_ms AS human_response_ms,
         (human_gap_ms > 300000) AS after_cache_5m, (human_gap_ms > 3600000) AS after_cache_60m
  FROM agent_turns;
"#;

/// v2: AI capacity (omp usage) per fetch.
const SCHEMA_V2: &str = r#"
CREATE TABLE capacity_snapshots (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, vendor TEXT, provider TEXT, account TEXT, limit_id TEXT, label TEXT,
  window_label TEXT, used_fraction REAL, resets_at INTEGER, status TEXT, UNIQUE(ts, provider, account, limit_id)
);
CREATE INDEX capacity_snapshots_ts ON capacity_snapshots(ts);
"#;

/// v3: assistant texts + FTS5 (trigram: substring search that works for Korean) over both sides.
const SCHEMA_V3: &str = r#"
CREATE TABLE assistant_messages (
  id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER NOT NULL REFERENCES sessions(id),
  ts INTEGER NOT NULL, chars INTEGER, text TEXT
);
CREATE INDEX assistant_messages_ts ON assistant_messages(ts);
CREATE VIRTUAL TABLE messages_fts USING fts5(text, rec_key UNINDEXED, session_id UNINDEXED, ts UNINDEXED, role UNINDEXED, tokenize = 'trigram');
"#;

pub struct Db {
    pub conn: Connection,
}

/// Open (creating + migrating) the store at `path`.
pub fn open_at(path: &Path) -> Res<Db> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;").map_err(|e| e.to_string())?;
    // model_family(model) for --by family and the reports
    conn.create_scalar_function(
        "model_family",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.filter(|m| !m.is_empty()).map(|m| model_family(&m))),
    )
    .map_err(|e| e.to_string())?;
    // norm_model(model): the write-time triggers (v16) store every model name normalized
    conn.create_scalar_function(
        "norm_model",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|m| if m.is_empty() { m } else { norm_model(&m) })),
    )
    .map_err(|e| e.to_string())?;
    migrate(&conn)?;
    Ok(Db { conn })
}

/// A read-only connection of its own (search, stats, lookups per request): long queries do
/// not hold the shared connection the poll loop and write routes use.
pub fn open_read() -> Res<Db> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA query_only=1;").map_err(|e| e.to_string())?;
    conn.create_scalar_function(
        "model_family",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.filter(|m| !m.is_empty()).map(|m| model_family(&m))),
    )
    .map_err(|e| e.to_string())?;
    Ok(Db { conn })
}

pub fn open() -> Res<Db> {
    open_at(&db_path())
}

fn migrate(conn: &Connection) -> Res<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if v > SCHEMA_VERSION {
        return Err(format!("analytics.db schema v{v} is newer than this adash (v{SCHEMA_VERSION})"));
    }
    if v < 1 {
        conn.execute_batch(&format!("BEGIN; {SCHEMA_V1} PRAGMA user_version = 1; COMMIT;")).map_err(|e| format!("migration v1: {e}"))?;
    }
    if v < 2 {
        conn.execute_batch(&format!("BEGIN; {SCHEMA_V2} PRAGMA user_version = 2; COMMIT;")).map_err(|e| format!("migration v2: {e}"))?;
    }
    if v < 3 {
        // full-text index; existing human messages go in now, assistant texts on the replay that
        // resetting the ingest offsets triggers (facts stay idempotent through their UNIQUE keys)
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V3}
             INSERT INTO messages_fts(text, rec_key, session_id, ts, role) SELECT text, rec_key, session_id, ts, 'user' FROM human_messages WHERE text IS NOT NULL;
             DELETE FROM ingest_files;
             PRAGMA user_version = 3; COMMIT;"
        ))
        .map_err(|e| format!("migration v3: {e}"))?;
    }
    if v < 4 {
        // review-script pointer lines were counted as human messages
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let keys: Vec<String> = {
            let mut st =
                tx.prepare("SELECT rec_key, text FROM human_messages WHERE kind = 'human' AND text LIKE '[%] @%를 읽고 답해줘%'").map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).filter(|(_, t)| is_peer_pointer(t)).map(|(k, _)| k).collect()
        };
        for k in &keys {
            tx.execute("UPDATE human_messages SET kind = 'peer', channel = 'peer' WHERE rec_key = ?1", [k]).map_err(|e| e.to_string())?;
            tx.execute("UPDATE agent_turns SET trigger_kind = 'peer' WHERE rec_key = ?1", [k]).map_err(|e| e.to_string())?;
        }
        tx.execute_batch("PRAGMA user_version = 4;").map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| format!("migration v4: {e}"))?;
    }
    if v < 5 {
        // agent→agent messages: senders recorded at send time (adash tell from an agent) and
        // recovered from agents' shell commands (adash tell / adash term send) on a replay
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE human_messages ADD COLUMN from_agent TEXT;
             CREATE TABLE agent_sends (
               id INTEGER PRIMARY KEY, rec_key TEXT NOT NULL UNIQUE, session_id INTEGER, ts INTEGER NOT NULL,
               from_agent TEXT, to_agent TEXT, channel TEXT NOT NULL, text TEXT NOT NULL
             );
             CREATE INDEX agent_sends_ts ON agent_sends(ts);
             DELETE FROM ingest_files;
             PRAGMA user_version = 5; COMMIT;",
        )
        .map_err(|e| format!("migration v5: {e}"))?;
    }
    if v < 6 {
        // tool call targets for the report's file / command records (replay fills them)
        conn.execute_batch("BEGIN; ALTER TABLE tool_calls ADD COLUMN target TEXT; DELETE FROM ingest_files; PRAGMA user_version = 6; COMMIT;")
            .map_err(|e| format!("migration v6: {e}"))?;
    }
    if v < 7 {
        // shell targets skip `cd …;` too: recompute on a replay
        conn.execute_batch("BEGIN; DELETE FROM ingest_files; PRAGMA user_version = 7; COMMIT;").map_err(|e| format!("migration v7: {e}"))?;
    }
    if v < 8 {
        // more review-script pointer formats ("… 를 읽고 지시대로 검토해줘") are agent requests
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let keys: Vec<String> = {
            let mut st = tx.prepare("SELECT rec_key, text FROM human_messages WHERE kind = 'human' AND text LIKE '[%] @%읽고%'").map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).filter(|(_, t)| is_peer_pointer(t)).map(|(k, _)| k).collect()
        };
        for k in &keys {
            tx.execute("UPDATE human_messages SET kind = 'peer', channel = 'peer' WHERE rec_key = ?1", [k]).map_err(|e| e.to_string())?;
            tx.execute("UPDATE agent_turns SET trigger_kind = 'peer' WHERE rec_key = ?1", [k]).map_err(|e| e.to_string())?;
        }
        tx.execute_batch("PRAGMA user_version = 8;").map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| format!("migration v8: {e}"))?;
    }
    if v < 9 {
        // edit targets from hashline headers, internal URIs dropped: recompute on a replay
        // + sessions launched as tests (adash launch API `test: true`) are kept out of awards
        conn.execute_batch(
            "BEGIN; ALTER TABLE sessions ADD COLUMN is_test INTEGER NOT NULL DEFAULT 0; DELETE FROM ingest_files; PRAGMA user_version = 9; COMMIT;",
        )
        .map_err(|e| format!("migration v9: {e}"))?;
    }
    if v < 10 {
        // omp skill invocations (custom_message skill-prompt) were not ingested as the user's
        // messages: replay; claude slash commands stored as <command-name> tags: keep the typed form
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = {
            let mut st = tx.prepare("SELECT rec_key, text FROM human_messages WHERE text LIKE '<command-%'").map_err(|e| e.to_string())?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
            r.filter_map(Result::ok).collect()
        };
        for (k, t) in rows {
            if let Some((typed, _)) = crate::transcript::claude_command(&t) {
                tx.execute(
                    "UPDATE human_messages SET text = ?2, chars = ?3, is_slash = ?4 WHERE rec_key = ?1",
                    params![k, typed, typed.chars().count() as i64, is_slash_command(&typed) as i64],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        tx.execute_batch("DELETE FROM ingest_files; PRAGMA user_version = 10;").map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| format!("migration v10: {e}"))?;
    }
    if v < 11 {
        // ask answers become my messages (terminal answers inserted, dashboard answers' text made
        // readable): replay
        conn.execute_batch("BEGIN; DELETE FROM ingest_files; PRAGMA user_version = 11; COMMIT;").map_err(|e| format!("migration v11: {e}"))?;
    }
    if v < 12 {
        // normalized model names (originals kept in model_raw); replay fills agy/codex turn models
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE agent_turns ADD COLUMN model_raw TEXT;
             ALTER TABLE usage_events ADD COLUMN model_raw TEXT;
             ALTER TABLE human_messages ADD COLUMN model_raw TEXT;
             ALTER TABLE tool_calls ADD COLUMN model_raw TEXT;
             DELETE FROM ingest_files;
             PRAGMA user_version = 12; COMMIT;",
        )
        .map_err(|e| format!("migration v12: {e}"))?;
    }
    if v < 13 {
        // codex usage was counted twice (one token_count per rate-limit id): drop codex usage
        // and re-read the codex files (turn totals are rewritten by the replay)
        conn.execute_batch(
            "BEGIN;
             DELETE FROM usage_events WHERE session_id IN (SELECT id FROM sessions WHERE agent_type = 'codex');
             DELETE FROM ingest_files WHERE path IN (SELECT file FROM sessions WHERE agent_type = 'codex');
             PRAGMA user_version = 13; COMMIT;",
        )
        .map_err(|e| format!("migration v13: {e}"))?;
    }
    if v < 14 {
        // partial indexes for the per-minute model pass
        conn.execute_batch(
            "BEGIN;
             CREATE INDEX IF NOT EXISTS agent_turns_model_new ON agent_turns(model) WHERE model_raw IS NULL;
             CREATE INDEX IF NOT EXISTS usage_events_model_new ON usage_events(model) WHERE model_raw IS NULL;
             CREATE INDEX IF NOT EXISTS human_messages_model_new ON human_messages(model) WHERE model_raw IS NULL;
             CREATE INDEX IF NOT EXISTS tool_calls_model_new ON tool_calls(model) WHERE model_raw IS NULL;
             CREATE INDEX IF NOT EXISTS agent_turns_model_missing ON agent_turns(session_id) WHERE model IS NULL OR model = '';
             PRAGMA user_version = 14; COMMIT;",
        )
        .map_err(|e| format!("migration v14: {e}"))?;
    }
    if v < 15 {
        // provider/API errors that ended an agent turn (backfilled once by `backfill_errors`)
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS errors (
                 id INTEGER PRIMARY KEY,
                 rec_key TEXT NOT NULL UNIQUE,
                 session_id INTEGER NOT NULL,
                 agent_type TEXT NOT NULL,
                 ts INTEGER NOT NULL,
                 provider TEXT,
                 model TEXT,
                 status INTEGER,
                 message TEXT NOT NULL,
                 recovered INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS errors_ts ON errors(ts);
             PRAGMA user_version = 15; COMMIT;",
        )
        .map_err(|e| format!("migration v15: {e}"))?;
    }
    if v < 16 {
        // every write path stores the normalized model (upserts used to overwrite it with the
        // raw name after the per-minute pass had run): triggers, and fix the rows left raw
        conn.execute_batch(
            "BEGIN;
             CREATE TRIGGER IF NOT EXISTS agent_turns_norm_ins AFTER INSERT ON agent_turns
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE agent_turns SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             CREATE TRIGGER IF NOT EXISTS agent_turns_norm_upd AFTER UPDATE OF model ON agent_turns
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE agent_turns SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             UPDATE agent_turns SET model_raw = model, model = norm_model(model) WHERE model IS NOT NULL AND model != '' AND model != norm_model(model);
             CREATE TRIGGER IF NOT EXISTS usage_events_norm_ins AFTER INSERT ON usage_events
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE usage_events SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             CREATE TRIGGER IF NOT EXISTS usage_events_norm_upd AFTER UPDATE OF model ON usage_events
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE usage_events SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             UPDATE usage_events SET model_raw = model, model = norm_model(model) WHERE model IS NOT NULL AND model != '' AND model != norm_model(model);
             CREATE TRIGGER IF NOT EXISTS human_messages_norm_ins AFTER INSERT ON human_messages
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE human_messages SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             CREATE TRIGGER IF NOT EXISTS human_messages_norm_upd AFTER UPDATE OF model ON human_messages
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE human_messages SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             UPDATE human_messages SET model_raw = model, model = norm_model(model) WHERE model IS NOT NULL AND model != '' AND model != norm_model(model);
             CREATE TRIGGER IF NOT EXISTS tool_calls_norm_ins AFTER INSERT ON tool_calls
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE tool_calls SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             CREATE TRIGGER IF NOT EXISTS tool_calls_norm_upd AFTER UPDATE OF model ON tool_calls
               WHEN NEW.model IS NOT NULL AND NEW.model != '' AND NEW.model != norm_model(NEW.model)
               BEGIN UPDATE tool_calls SET model_raw = NEW.model, model = norm_model(NEW.model) WHERE id = NEW.id; END;
             UPDATE tool_calls SET model_raw = model, model = norm_model(model) WHERE model IS NOT NULL AND model != '' AND model != norm_model(model);
             PRAGMA user_version = 16; COMMIT;",
        )
        .map_err(|e| format!("migration v16: {e}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// classification helpers

/// ko / en / mixed by letter counts (Hangul vs Latin).
pub fn lang_guess(t: &str) -> &'static str {
    let ko = t.chars().filter(|c| ('\u{AC00}'..='\u{D7A3}').contains(c) || ('\u{3131}'..='\u{318E}').contains(c)).count();
    let en = t.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let total = ko + en;
    if total == 0 {
        "none"
    } else if ko * 10 >= total * 7 {
        "ko"
    } else if ko * 10 <= total {
        "en"
    } else {
        "mixed"
    }
}

/// `/compact`, `/model opus` … but not a path like `/home/x/y.md 를 읽고`.
pub fn is_slash_command(t: &str) -> bool {
    let t = t.trim_start();
    let Some(rest) = t.strip_prefix('/') else { return false };
    let tok = rest.split_whitespace().next().unwrap_or("");
    !tok.is_empty()
        && !tok.contains('/')
        && !tok.contains('.')
        && tok.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
}

/// peer_review.py / magi.py pointer line: `[marker] @brief 를 읽고 답해줘.` (sent by an agent)
pub fn is_peer_pointer(t: &str) -> bool {
    // "[marker] @brief 를 읽고 답해줘." and variants ("… 를 읽고 지시대로 검토해줘")
    t.starts_with('[')
        && t.split_once("] @").is_some_and(|(m, rest)| {
            !m.contains('\n') && m.len() <= 120 && rest.split_once(' ').is_some_and(|(_, r)| r.starts_with("를 읽고") || r.starts_with("을 읽고"))
        })
}

fn zstd_available() -> bool {
    std::process::Command::new("sh").args(["-c", "command -v zstd"]).output().is_ok_and(|o| o.status.success())
}

/// `src` → `dst` with zstd (src removed); without zstd the plain copy is renamed to `.db`.
fn compress(src: &Path, dst: &Path) -> Res<PathBuf> {
    if zstd_available() {
        let st = crate::platform::timeout(600).args(["zstd", "-q", "-10", "-T0", "--rm", "-f", "-o"]).arg(dst).arg(src).status().map_err(|e| e.to_string())?;
        if st.success() {
            return Ok(dst.to_path_buf());
        }
    }
    let plain = dst.with_extension("");
    std::fs::rename(src, &plain).map_err(|e| e.to_string())?;
    Ok(plain)
}

/// Hash of a file's first 4 KiB (rewrite detection).
fn head_hash(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 4096];
    let n = f.read(&mut buf).ok()?;
    Some(format!("{:x}", Sha256::digest(&buf[..n]))[..16].to_string())
}

/// One model name for aggregation: provider prefix (`anthropic/`, `openai-codex/`, `zai/`, …),
/// thinking/effort suffix (`:low`, `-medium` on gemini), date snapshots (`-20251001`, `@…`)
/// removed; lowercase. The original stays in `model_raw`.
pub fn norm_model(raw: &str) -> String {
    let m = raw.trim().to_lowercase();
    let m = m.rsplit('/').next().unwrap_or(&m).to_string();
    let m = m.split([':', '@']).next().unwrap_or("").to_string();
    static DATE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"-(20\d{6}|\d{4}-\d{2}-\d{2})$").unwrap());
    let m = DATE.replace(&m, "").to_string();
    static EFFORT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^(.+?)-(minimal|low|medium|high|xhigh)$").unwrap());
    EFFORT.replace(&m, "$1").to_string()
}

/// Model family: opus, sonnet, haiku, fable, mythos, gpt, gemini, glm, … (else the name).
pub fn model_family(norm: &str) -> String {
    for f in ["opus", "sonnet", "haiku", "fable", "mythos", "gemini", "glm", "deepseek", "qwen", "kimi", "grok"] {
        if norm.contains(f) {
            return f.into();
        }
    }
    if norm.starts_with("gpt") || norm.starts_with("o3") || norm.starts_with("o4") || norm.contains("codex") {
        return "gpt".into();
    }
    norm.split('-').next().unwrap_or(norm).to_string()
}

/// File headers of an omp hashline patch: lines `[PATH#ABCD]` (4 hex digits).
pub fn hashline_paths(input: &str) -> Vec<String> {
    input
        .lines()
        .filter_map(|l| {
            let inner = l.trim().strip_prefix('[')?.strip_suffix(']')?;
            let (p, tag) = inner.rsplit_once('#')?;
            (tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit()) && !p.is_empty()).then(|| p.trim_matches('"').to_string())
        })
        .collect()
}

/// What a tool call touched, for the report (several files: one `file:` line each): `file:<path>` for read/edit/write tools, `cmd:<first
/// word>` for shell tools (leading env assignments and `cd … &&` skipped).
pub fn tool_target(tool: &str, args: &Value) -> Option<String> {
    let t = tool.to_ascii_lowercase();
    let s = |k: &str| args[k].as_str().map(str::to_string);
    if matches!(
        t.as_str(),
        "read"
            | "read_file"
            | "view"
            | "edit"
            | "multiedit"
            | "write"
            | "write_file"
            | "notebookedit"
            | "apply_patch"
            | "str_replace_based_edit_tool"
            | "replace"
            | "ast_edit"
    ) {
        // omp hashline edits: `input` holds one `[PATH#TAG]` header per patched file
        let mut paths: Vec<String> = args["input"].as_str().map(hashline_paths).unwrap_or_default();
        if paths.is_empty() {
            if let Some(p) = s("path")
                .or_else(|| s("file_path"))
                .or_else(|| s("filePath"))
                .or_else(|| s("absolute_path"))
                .or_else(|| s("AbsolutePath"))
                .or_else(|| s("TargetFile"))
            {
                paths.push(p);
            }
            paths.extend(args["paths"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)));
        }
        let paths: Vec<String> = paths
            .into_iter()
            // internal URIs (xd://, agent://, proc://, skill://, …) are not files
            .filter(|p| !p.contains("://") && !p.is_empty())
            // read selectors like `notes.txt:10-20`
            .map(|p| {
                p.split_once(':')
                    .filter(|(a, b)| !a.is_empty() && b.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-' || c == 'r'))
                    .map(|x| x.0.to_string())
                    .unwrap_or(p)
            })
            .collect();
        if paths.is_empty() {
            return None;
        }
        return Some(paths.iter().map(|p| format!("file:{p}")).collect::<Vec<_>>().join("\n"));
    }
    if matches!(t.as_str(), "bash" | "shell" | "exec_command" | "local_shell" | "shell_command" | "run_command" | "exec") {
        let cmd = match (&args["command"], &args["cmd"], &args["CommandLine"]) {
            (Value::String(c), _, _) | (_, Value::String(c), _) | (_, _, Value::String(c)) => c.clone(),
            (Value::Array(a), _, _) | (_, Value::Array(a), _) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "),
            _ => return None,
        };
        let mut seg = cmd.as_str();
        // skip leading `cd somewhere &&` / `cd somewhere;` / `cd somewhere` + newline
        while seg.trim_start().starts_with("cd ") {
            match seg.find([';', '\n']).map(|i| (i, 1)).into_iter().chain(seg.find("&&").map(|i| (i, 2))).min_by_key(|x| x.0) {
                Some((i, n)) => seg = &seg[i + n..],
                None => break,
            }
        }
        let word = seg.split_whitespace().find(|w| !w.contains('=') && *w != "sudo" && *w != "time")?;
        let word = word.rsplit('/').next().unwrap_or(word).trim_matches(['(', ')', '"', '\'']);
        return (!word.is_empty()).then(|| format!("cmd:{word}"));
    }
    None
}

/// Requester in an adash relay line: `[adash T… from NAME] …` / `[adash T… reply from NAME] …`.
pub fn relay_from(t: &str) -> Option<String> {
    let head = t.strip_prefix("[adash ")?.split_once(']')?.0;
    let from = head.rsplit_once(" from ")?.1.trim();
    (!from.is_empty()).then(|| from.to_string())
}

/// human | relay | adash | peer (line prefixes adash or the review scripts write into prompts)
pub fn message_kind(t: &str, subagent: bool) -> &'static str {
    if subagent {
        "subagent"
    } else if t.starts_with("[adash] 사용자 메시지") {
        "human"
    } else if t.starts_with("[adash] ") {
        "adash"
    } else if t.starts_with("[adash ") {
        "relay"
    } else if is_peer_pointer(t) {
        "peer"
    } else {
        "human"
    }
}

fn attachment_count(t: &str) -> i64 {
    crate::transcript::attachments_of(t).map(|v| v.len() as i64).unwrap_or(0)
}

fn ts_of(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis()),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

fn codename(title: &str) -> Option<String> {
    let t = crate::session::clean_title(title);
    let re = regex::Regex::new(r"\b([A-Z][A-Z0-9]{1,7}):\s").ok()?;
    re.captures(&t).map(|c| c[1].to_string())
}

// ---------------------------------------------------------------------------------------------
// ingestion context

/// A dashboard/CLI send logged in history.jsonl, matched against session user messages.
#[derive(Clone)]
pub struct Send {
    pub ts: i64,
    pub text40: String,
    pub channel: String,
}

#[derive(Default)]
pub struct Ctx {
    /// (worktree path, project name), longest prefix wins
    pub projects: Vec<(String, String)>,
    /// session file → (live adash name, handle, mapping confidence)
    pub live: HashMap<PathBuf, (String, String, String)>,
    pub sends: Vec<Send>,
}

/// A complete line that did not parse at `off`: true while it should be retried (the same place
/// failed fewer than `JsonlPos::TORN_RETRIES` rounds).
fn torn_wait(file: &str, off: u64) -> bool {
    use crate::util::LockExt;
    static TORN: std::sync::LazyLock<std::sync::Mutex<HashMap<String, (u64, u8)>>> = std::sync::LazyLock::new(Default::default);
    let mut t = TORN.plock();
    let e = t.entry(file.to_string()).or_insert((off, 0));
    if e.0 != off {
        *e = (off, 0);
    }
    e.1 += 1;
    e.1 < crate::util::JsonlPos::TORN_RETRIES
}

impl Ctx {
    fn project_of(&self, cwd: &str) -> (String, Option<String>) {
        let best = self.projects.iter().filter(|(p, _)| cwd == p || cwd.starts_with(&format!("{}/", p.trim_end_matches('/')))).max_by_key(|(p, _)| p.len());
        match best {
            Some((p, n)) => (n.clone(), Some(p.clone())),
            None => (Path::new(cwd).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), None),
        }
    }

    fn channel_of(&self, ts: i64, text: &str) -> &str {
        let key: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
        self.sends.iter().find(|s| (ts - s.ts).abs() <= 300_000 && s.text40 == key).map(|s| s.channel.as_str()).unwrap_or("terminal")
    }
}

// ---------------------------------------------------------------------------------------------
// per-file incremental state

#[derive(Serialize, Deserialize, Default, Clone)]
struct Turn {
    key: String,
    trigger: String,
    start: i64,
    end: i64,
    gap: Option<i64>,
    agent_name: Option<String>,
    model: Option<String>,
    chars: i64,
    tools: i64,
    spawns: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
    interrupted: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct FileState {
    session_id: i64,
    model: Option<String>,
    code: Option<String>,
    origin: Option<String>,
    turn: Option<Turn>,
    /// agent mid-turn (assistant asked for tools / task not complete): a user message now steers
    busy: bool,
    last_turn_end: Option<i64>,
    /// call id → (ts, rec_key)
    pending: HashMap<String, (i64, String)>,
    /// open ask call id
    ask: Option<String>,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    titles: Vec<String>,
    models: Vec<String>,
    /// codex: last `total_token_usage.total_tokens` counted (it writes the same token_count
    /// twice, once per rate-limit id)
    #[serde(default)]
    codex_total: Option<i64>,
    /// hash of the file's first 4 KiB: a rewritten file (same path, new content) is replayed
    /// from scratch instead of appending duplicate offset-keyed rows
    #[serde(default)]
    head: Option<String>,
}

struct SessionMeta {
    file: String,
    agent_type: &'static str,
    parent_file: Option<String>,
    subagent: bool,
}

pub struct Ingest<'a> {
    db: &'a Db,
    ctx: &'a Ctx,
}

#[derive(Default, Debug, Clone, Copy, Serialize)]
pub struct Counts {
    pub files: usize,
    pub bytes: u64,
}

impl<'a> Ingest<'a> {
    pub fn new(db: &'a Db, ctx: &'a Ctx) -> Self {
        Ingest { db, ctx }
    }

    /// Ingest every session file under the given omp/claude/codex roots (incremental).
    pub fn all_sessions(&self) -> Res<Counts> {
        let mut c = Counts::default();
        let mut files: Vec<(PathBuf, SessionMetaKind)> = Vec::new();
        let root = |k: &str| crate::harness::get(k).and_then(|h| h.session_roots().into_iter().next()).unwrap_or_default();
        let omp = root("omp");
        walk(&omp, &mut |p| {
            let rel_depth = p.strip_prefix(&omp).map(|r| r.components().count()).unwrap_or(0);
            if rel_depth == 2 {
                files.push((p.to_path_buf(), SessionMetaKind::Omp));
            } else if rel_depth >= 3 {
                files.push((p.to_path_buf(), SessionMetaKind::OmpSub));
            }
        });
        walk(&root("claude"), &mut |p| files.push((p.to_path_buf(), SessionMetaKind::Claude)));
        walk(&root("codex"), &mut |p| files.push((p.to_path_buf(), SessionMetaKind::Codex)));
        // agy conversation dbs (not jsonl): handled by `agy_db`
        if let Ok(rd) = std::fs::read_dir(crate::agy::root().join("conversations")) {
            let workspaces = agy_workspaces();
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "db") {
                    // one bad file must not stop the pass
                    match self.agy_db(&p, &workspaces) {
                        Ok(Some(n)) => {
                            c.files += 1;
                            c.bytes += n;
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("adash: ingest {}: {e}", p.display()),
                    }
                }
            }
        }
        // omp btw side questions (their own files; the session jsonl does not change)
        for (p, k) in &files {
            if *k == SessionMetaKind::Omp {
                match self.btw(p) {
                    Ok(n) => c.files += n as usize,
                    Err(e) => eprintln!("adash: ingest btw {}: {e}", p.display()),
                }
            }
        }
        files.sort();
        for (p, k) in files {
            match self.file(&p, k) {
                Ok(Some(n)) => {
                    c.files += 1;
                    c.bytes += n;
                }
                Ok(None) => {}
                Err(e) => eprintln!("adash: ingest {}: {e}", p.display()),
            }
        }
        Ok(c)
    }

    /// btw entries of one omp session: my question (human_messages, channel "btw") and the
    /// side turn (agent_turns, trigger "btw"). Skipped when the directory has not changed.
    pub fn btw(&self, session: &Path) -> Res<u64> {
        let sig = crate::btw::sig(session);
        if sig.is_empty() {
            return Ok(0);
        }
        let key = format!("{}#btw", session.display());
        let seen: Option<String> =
            self.db.conn.query_row("SELECT state FROM ingest_files WHERE path = ?1", [&key], |r| r.get(0)).optional().map_err(|e| e.to_string())?.flatten();
        if seen.as_deref() == Some(sig.as_str()) {
            return Ok(0);
        }
        let file = session.to_string_lossy().into_owned();
        let Some((sid, project, name)) = self
            .db
            .conn
            .query_row("SELECT id, project, json_extract(names, '$[#-1]') FROM sessions WHERE file = ?1", [&file], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
            })
            .optional()
            .map_err(|e| e.to_string())?
        else {
            return Ok(0); // session not ingested yet: next pass
        };
        let tx = self.db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        for e in crate::btw::load(session).into_iter().flat_map(|e| {
            let f = e.follow_ups.clone();
            std::iter::once(e).chain(f)
        }) {
            let hk = format!("{file}#btw:{}", e.id);
            let text = &e.question;
            let n = self
                .db
                .conn
                .execute(
                    "INSERT INTO human_messages(rec_key, session_id, ts, project, agent_name, agent_type, kind, channel, chars, lines, lang, attachments, is_slash, steering, text, sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'omp', 'human', 'btw', ?6, ?7, ?8, 0, 0, 0, ?9, ?10) ON CONFLICT(rec_key) DO NOTHING",
                    params![hk, sid, e.created, project, name, text.chars().count() as i64, text.lines().count().max(1) as i64, lang_guess(text), text, format!("{:x}", Sha256::digest(text.as_bytes()))],
                )
                .map_err(|e| e.to_string())?;
            if n > 0 {
                self.index_text(&hk, sid, e.created, "user", text)?;
            }
            if e.q_status() != "pending" {
                self.db
                    .conn
                    .execute(
                        "INSERT INTO agent_turns(rec_key, session_id, trigger_kind, start_ts, end_ts, duration_ms, project, agent_name, agent_type, assistant_chars, interrupted)
                         VALUES (?1, ?2, 'btw', ?3, ?4, ?5, ?6, ?7, 'omp', ?8, ?9)
                         ON CONFLICT(rec_key) DO UPDATE SET end_ts = ?4, duration_ms = ?5, assistant_chars = ?8, interrupted = ?9",
                        params![format!("{file}#btwturn:{}", e.id), sid, e.created, e.updated, (e.updated - e.created).max(0), project, name, e.answer.chars().count() as i64, (e.q_status() == "error") as i64],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        self.db
            .conn
            .execute(
                "INSERT INTO ingest_files(path, source, size, mtime, offset, state, updated_at) VALUES (?1, 'omp-btw', 0, 0, 0, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET state = ?2, updated_at = ?3",
                params![key, sig, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(1)
    }

    /// One agy conversation db: steps after the stored idx (kept in `offset`). Returns steps read.
    pub fn agy_db(&self, path: &Path, workspaces: &HashMap<String, String>) -> Res<Option<u64>> {
        let key = path.to_string_lossy().into_owned();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mtime = crate::util::mtime_ms(path);
        let conn = &self.db.conn;
        let row: Option<(i64, i64, Option<String>)> = conn
            .query_row("SELECT mtime, offset, state FROM ingest_files WHERE path = ?1", [&key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(|e| e.to_string())?;
        if row.as_ref().is_some_and(|r| r.0 == mtime) {
            return Ok(None);
        }
        let (after, mut st): (i64, FileState) = match &row {
            Some((_, o, s)) => (*o, s.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default()),
            None => (0, FileState::default()),
        };
        let steps = match crate::agy::read_steps(path, after) {
            Ok(s) => s,
            Err(_) => return Ok(None), // locked/corrupt: try again next round
        };
        let meta = SessionMeta { file: key.clone(), agent_type: "antigravity", parent_file: None, subagent: false };
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        if st.model.is_none() {
            if let Some(m) = crate::agy::model_of(path) {
                st.models.push(m.clone());
                st.model = Some(m);
            }
        }
        if st.session_id == 0 {
            st.session_id = self.session_row(&meta)?;
            let conv = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(cwd) = workspaces.get(&conv) {
                self.set_cwd(st.session_id, cwd, Some(&conv))?;
            }
        }
        let mut last = after;
        for s in &steps {
            last = last.max(s.idx);
            if s.ts > 0 {
                st.first_ts = Some(st.first_ts.map_or(s.ts, |f: i64| f.min(s.ts)));
                st.last_ts = Some(st.last_ts.map_or(s.ts, |l: i64| l.max(s.ts)));
            }
            let auto = format!("{key}#auto{}", s.idx);
            match s.kind {
                crate::agy::USER => {
                    let text = s.text.clone().unwrap_or_default();
                    self.user_message(&meta, &mut st, format!("{key}#{}", s.idx), s.ts, &text, 0)?;
                }
                crate::agy::MODEL => {
                    // agent output continues the turn (agy has no explicit "turn done" step)
                    st.busy = st.busy || st.turn.is_some();
                    self.activity(&meta, &mut st, s.ts, &auto)?;
                    if let Some(t) = st.turn.as_mut() {
                        t.chars += s.text.as_deref().map(|x| x.chars().count() as i64).unwrap_or(0);
                    }
                    self.assistant_text(&st, format!("{key}#{}", s.idx), s.ts, s.text.as_deref().unwrap_or(""))?;
                    // a finished model step may be the final answer: a user step right after it starts a
                    // new turn, while a following tool step (busy again) keeps the same turn
                    if s.status == crate::agy::FINISHED {
                        st.busy = false;
                    }
                }
                crate::agy::TOOL => {
                    st.busy = st.busy || st.turn.is_some();
                    self.activity(&meta, &mut st, s.ts, &auto)?;
                    if let Some((id, name, _)) = &s.tool {
                        let cid = if id.is_empty() { format!("step{}", s.idx) } else { id.clone() };
                        let args = s.tool.as_ref().map(|t| t.2.clone()).unwrap_or(Value::Null);
                        self.tool_call(&mut st, &key, &cid, s.ts, name, &args)?;
                    }
                    st.busy = true;
                }
                _ => {}
            }
        }
        self.flush_turn(&meta, &st)?;
        self.update_session(&meta, &st)?;
        conn.execute(
            "INSERT INTO ingest_files(path, source, size, mtime, offset, state, updated_at) VALUES (?1, 'antigravity', ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET size = ?2, mtime = ?3, offset = ?4, state = ?5, updated_at = ?6",
            params![key, size as i64, mtime, last, serde_json::to_string(&st).unwrap(), now_ms()],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(steps.len() as u64))
    }

    /// Ingest new bytes of one session file. Returns bytes read (None when unchanged).
    pub fn file(&self, path: &Path, kind: SessionMetaKind) -> Res<Option<u64>> {
        let md = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        let size = md.len();
        let mtime = crate::util::mtime_ms(path);
        let key = path.to_string_lossy().into_owned();
        let conn = &self.db.conn;
        let row: Option<(i64, i64, Option<String>)> = conn
            .query_row("SELECT size, offset, state FROM ingest_files WHERE path = ?1", [&key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(|e| e.to_string())?;
        if row.as_ref().is_some_and(|r| r.0 as u64 == size && r.1 as u64 == size) {
            return Ok(None); // unchanged size, fully read
        }
        let head = head_hash(path);
        let (mut offset, mut st) = match &row {
            Some((s, o, state)) if *s as u64 <= size => {
                (*o as u64, state.as_deref().and_then(|s| serde_json::from_str::<FileState>(s).ok()).unwrap_or_default())
            }
            _ => (0, FileState::default()),
        };
        // shrunk, or the beginning changed: the file was rewritten — drop what it produced and
        // read it again from the start
        let rewritten = row.as_ref().is_some_and(|r| (r.0 as u64) > size) || (st.head.is_some() && st.head != head);
        if rewritten {
            let sid = st.session_id.max(conn.query_row("SELECT id FROM sessions WHERE file = ?1", [&key], |r| r.get(0)).unwrap_or(0));
            if sid > 0 {
                for t in ["human_messages", "agent_turns", "tool_calls", "usage_events", "asks", "assistant_messages", "agent_sends", "errors"] {
                    let _ = conn.execute(&format!("DELETE FROM {t} WHERE session_id = ?1"), [sid]);
                }
                let _ = conn.execute("DELETE FROM messages_fts WHERE session_id = ?1", [sid]);
            }
            offset = 0;
            st = FileState { session_id: sid, ..FileState::default() };
        }
        st.head = head;
        let meta = SessionMeta {
            file: key.clone(),
            agent_type: kind.agent_type(),
            parent_file: (kind == SessionMetaKind::OmpSub).then(|| path.parent().map(|d| format!("{}.jsonl", d.display()))).flatten(),
            subagent: kind == SessionMetaKind::OmpSub,
        };
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        if st.session_id == 0 {
            st.session_id = self.session_row(&meta)?;
        }
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut r = BufReader::with_capacity(1 << 20, f);
        r.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let start = offset;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = r.read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
            if n == 0 || line.last() != Some(&b'\n') {
                break; // partial last line: wait for the writer
            }
            let rec_off = offset;
            offset += n as u64;
            if !relevant(kind, &line) {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(&line) else {
                // read torn (newline visible before the bytes ahead of it): retry next round,
                // skip after a few
                if torn_wait(&key, rec_off) {
                    offset = rec_off;
                    break;
                }
                eprintln!("adash: {key}: skipping an unreadable line at byte {rec_off}");
                continue;
            };
            self.record(&meta, kind, &v, rec_off, &mut st)?;
            self.db.error_row(meta.agent_type, &meta.file, st.session_id, &v, rec_off, st.model.as_deref())?;
        }
        self.flush_turn(&meta, &st)?;
        self.update_session(&meta, &st)?;
        conn.execute(
            "INSERT INTO ingest_files(path, source, size, mtime, offset, state, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET size = ?3, mtime = ?4, offset = ?5, state = ?6, updated_at = ?7",
            params![key, meta.agent_type, size as i64, mtime, offset as i64, serde_json::to_string(&st).unwrap(), now_ms()],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(offset - start))
    }

    fn session_row(&self, m: &SessionMeta) -> Res<i64> {
        let conn = &self.db.conn;
        conn.execute(
            "INSERT INTO sessions(file, agent_type, parent_file, is_subagent) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(file) DO NOTHING",
            params![m.file, m.agent_type, m.parent_file, m.subagent as i64],
        )
        .map_err(|e| e.to_string())?;
        conn.query_row("SELECT id FROM sessions WHERE file = ?1", [&m.file], |r| r.get(0)).map_err(|e| e.to_string())
    }

    fn update_session(&self, m: &SessionMeta, st: &FileState) -> Res<()> {
        let live = self.ctx.live.get(Path::new(&m.file));
        let conn = &self.db.conn;
        let add_json = |col: &str, val: Option<&str>| -> Res<()> {
            let Some(val) = val else { return Ok(()) };
            let cur: String = conn.query_row(&format!("SELECT {col} FROM sessions WHERE id = ?1"), [st.session_id], |r| r.get(0)).map_err(|e| e.to_string())?;
            let mut list: Vec<String> = serde_json::from_str(&cur).unwrap_or_default();
            if !list.iter().any(|x| x == val) {
                list.push(val.to_string());
                conn.execute(&format!("UPDATE sessions SET {col} = ?1 WHERE id = ?2"), params![serde_json::to_string(&list).unwrap(), st.session_id])
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        for t in &st.titles {
            add_json("titles", Some(t))?;
        }
        for x in &st.models {
            add_json("models", Some(x))?;
        }
        if let Some((name, handle, conf)) = live {
            add_json("names", Some(name))?;
            add_json("handles", Some(handle))?;
            conn.execute("UPDATE sessions SET mapping = ?1 WHERE id = ?2", params![conf, st.session_id]).map_err(|e| e.to_string())?;
        }
        conn.execute(
            "UPDATE sessions SET first_ts = COALESCE(first_ts, ?1), last_ts = MAX(COALESCE(last_ts, 0), COALESCE(?2, 0)), origin = COALESCE(?3, origin) WHERE id = ?4",
            params![st.first_ts, st.last_ts, st.origin, st.session_id],
        )
        .map_err(|e| e.to_string())?;
        let _ = m;
        Ok(())
    }

    fn session_info(&self, sid: i64) -> (Option<String>, Option<String>) {
        self.db.conn.query_row("SELECT project, cwd FROM sessions WHERE id = ?1", [sid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or((None, None))
    }

    fn set_cwd(&self, sid: i64, cwd: &str, uuid: Option<&str>) -> Res<()> {
        let (project, wt) = self.ctx.project_of(cwd);
        self.db
            .conn
            .execute(
                "UPDATE sessions SET cwd = ?1, project = ?2, worktree = ?3, session_uuid = COALESCE(?4, session_uuid) WHERE id = ?5",
                params![cwd, project, wt, uuid, sid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn agent_name(&self, m: &SessionMeta, st: &FileState) -> Option<String> {
        self.ctx.live.get(Path::new(&m.file)).map(|l| l.0.clone()).or_else(|| st.code.clone())
    }

    fn user_message(&self, m: &SessionMeta, st: &mut FileState, key: String, ts: i64, text: &str, extra_attachments: i64) -> Res<()> {
        let kind = message_kind(text, m.subagent);
        // a human message while the agent is mid-turn steers it; anything else starts a new turn
        let steering = st.busy && kind == "human" && st.turn.is_some();
        if !steering {
            if let Some(t) = st.turn.take() {
                self.write_turn(m, st, &t)?;
                st.last_turn_end = Some(t.end.max(t.start));
            }
        }
        let channel = if kind == "human" { self.ctx.channel_of(ts, text).to_string() } else { kind.to_string() };
        let (project, _) = self.session_info(st.session_id);
        self.db
            .conn
            .execute(
                "INSERT INTO human_messages(rec_key, session_id, ts, project, agent_name, agent_type, model, kind, channel, chars, lines, lang, attachments, is_slash, steering, text, sha256)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17) ON CONFLICT(rec_key) DO NOTHING",
                params![
                    key,
                    st.session_id,
                    ts,
                    project,
                    self.agent_name(m, st),
                    m.agent_type,
                    st.model,
                    kind,
                    channel,
                    text.chars().count() as i64,
                    text.lines().count().max(1) as i64,
                    lang_guess(text),
                    attachment_count(text) + extra_attachments,
                    is_slash_command(text) as i64,
                    steering as i64,
                    text,
                    format!("{:x}", Sha256::digest(text.as_bytes())),
                ],
            )
            .map_err(|e| e.to_string())
            .and_then(|n| if n > 0 { self.index_text(&key, st.session_id, ts, "user", text) } else { Ok(()) })?;
        if steering {
            // steering joins the running turn instead of starting a new one
            st.turn = st.turn.take().or_else(|| {
                Some(Turn {
                    key: key.clone(),
                    trigger: kind.into(),
                    start: ts,
                    end: ts,
                    model: st.model.clone(),
                    agent_name: self.agent_name(m, st),
                    ..Default::default()
                })
            });
        } else {
            st.turn = Some(Turn {
                key,
                trigger: kind.into(),
                start: ts,
                end: ts,
                gap: st.last_turn_end.map(|e| ts - e).filter(|g| *g >= 0),
                agent_name: self.agent_name(m, st),
                model: st.model.clone(),
                ..Default::default()
            });
        }
        st.busy = true;
        Ok(())
    }

    fn index_text(&self, key: &str, sid: i64, ts: i64, role: &str, text: &str) -> Res<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.db
            .conn
            .execute("INSERT INTO messages_fts(text, rec_key, session_id, ts, role) VALUES (?1,?2,?3,?4,?5)", params![text, key, sid, ts, role])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Final/visible assistant text (no tool calls, no thinking) → `assistant_messages` + FTS.
    fn assistant_text(&self, st: &FileState, key: String, ts: i64, text: &str) -> Res<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let n = self
            .db
            .conn
            .execute(
                "INSERT INTO assistant_messages(rec_key, session_id, ts, chars, text) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(rec_key) DO NOTHING",
                params![key, st.session_id, ts, text.chars().count() as i64, text],
            )
            .map_err(|e| e.to_string())?;
        if n > 0 {
            self.index_text(&key, st.session_id, ts, "assistant", text)?;
        }
        Ok(())
    }

    fn write_turn(&self, _m: &SessionMeta, st: &FileState, t: &Turn) -> Res<()> {
        let (project, _) = self.session_info(st.session_id);
        let end = t.end.max(t.start);
        self.db
            .conn
            .execute(
                "INSERT INTO agent_turns(rec_key, session_id, trigger_kind, start_ts, end_ts, duration_ms, human_gap_ms, project, agent_name, agent_type, model,
                   assistant_chars, tool_calls, subagent_spawns, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost, interrupted)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,(SELECT agent_type FROM sessions WHERE id=?2),?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
                 ON CONFLICT(rec_key) DO UPDATE SET end_ts=?5, duration_ms=?6, model=?10, assistant_chars=?11, tool_calls=?12, subagent_spawns=?13,
                   input_tokens=?14, output_tokens=?15, cache_read_tokens=?16, cache_write_tokens=?17, cost=?18, interrupted=?19",
                params![
                    t.key, st.session_id, t.trigger, t.start, end, end - t.start, t.gap, project, t.agent_name, t.model, t.chars, t.tools, t.spawns,
                    t.input, t.output, t.cache_read, t.cache_write, t.cost, t.interrupted as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn flush_turn(&self, m: &SessionMeta, st: &FileState) -> Res<()> {
        if let Some(t) = &st.turn {
            self.write_turn(m, st, t)?;
        }
        Ok(())
    }

    /// Agent output. Extends the running turn; output after the turn ended (async results,
    /// background-job notices resuming the agent) starts a separate `auto` turn instead of
    /// stretching the finished one across hours.
    fn activity(&self, m: &SessionMeta, st: &mut FileState, ts: i64, key: &str) -> Res<()> {
        if !st.busy && st.turn.is_some() {
            if let Some(t) = st.turn.take() {
                self.write_turn(m, st, &t)?;
                st.last_turn_end = Some(t.end.max(t.start));
            }
            st.turn = Some(Turn {
                key: key.to_string(),
                trigger: "auto".into(),
                start: ts,
                end: ts,
                agent_name: self.agent_name(m, st),
                model: st.model.clone(),
                ..Default::default()
            });
            st.busy = true;
        }
        if let Some(t) = st.turn.as_mut() {
            t.end = t.end.max(ts);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn usage(
        &self,
        st: &mut FileState,
        key: String,
        ts: i64,
        purpose: &str,
        provider: Option<&str>,
        model: Option<&str>,
        i: i64,
        o: i64,
        cr: i64,
        cw: i64,
        cost: f64,
        in_turn: bool,
    ) -> Res<()> {
        let (project, _) = self.session_info(st.session_id);
        self.db
            .conn
            .execute(
                "INSERT INTO usage_events(rec_key, session_id, ts, purpose, provider, model, project, agent_type, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,(SELECT agent_type FROM sessions WHERE id=?2),?8,?9,?10,?11,?12) ON CONFLICT(rec_key) DO NOTHING",
                params![key, st.session_id, ts, purpose, provider, model, project, i, o, cr, cw, cost],
            )
            .map_err(|e| e.to_string())?;
        // count even when the row already existed: a replay rebuilds the whole turn
        if in_turn {
            if let Some(t) = st.turn.as_mut() {
                t.input += i;
                t.output += o;
                t.cache_read += cr;
                t.cache_write += cw;
                t.cost += cost;
            }
        }
        Ok(())
    }

    /// A shell command that messages another agent (`adash tell`, `adash term send`, and
    /// the sender side of an agent→agent message,
    /// matched to the receiver's user message later.
    fn shell_send(&self, m: &SessionMeta, st: &FileState, file: &str, call_id: &str, ts: i64, args: &Value) -> Res<()> {
        let args = match args {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone())),
            v => v.clone(),
        };
        let cmd = match (&args["command"], &args["cmd"], &args) {
            (Value::String(c), _, _) | (_, Value::String(c), _) | (_, _, Value::String(c)) => c.clone(),
            (Value::Array(a), _, _) | (_, Value::Array(a), _) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "),
            _ => return Ok(()),
        };
        let channel = if cmd.contains("adash tell ") {
            "cli-tell"
        } else if cmd.contains("adash term send ") {
            "term-send"
        } else {
            return Ok(());
        };
        self.db
            .conn
            .execute(
                "INSERT INTO agent_sends(rec_key, session_id, ts, from_agent, channel, text) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(rec_key) DO NOTHING",
                params![format!("{file}#send:{call_id}"), st.session_id, ts, self.agent_name(m, st), channel, cmd],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn tool_call(&self, st: &mut FileState, file: &str, call_id: &str, ts: i64, tool: &str, args: &Value) -> Res<()> {
        let target = tool_target(tool, args);
        let key = format!("{file}#tool:{call_id}");
        let (project, _) = self.session_info(st.session_id);
        let turn_key = st.turn.as_ref().map(|t| t.key.clone());
        self.db
            .conn
            .execute(
                "INSERT INTO tool_calls(rec_key, session_id, turn_key, ts, tool, project, agent_type, model, target)
                 VALUES (?1,?2,?3,?4,?5,?6,(SELECT agent_type FROM sessions WHERE id=?2),?7,?8) ON CONFLICT(rec_key) DO UPDATE SET target = excluded.target",
                params![key, st.session_id, turn_key, ts, tool, project, st.model, target],
            )
            .map_err(|e| e.to_string())?;
        // count even when the row already existed: a replay rebuilds the whole turn
        if let Some(t) = st.turn.as_mut() {
            t.tools += 1;
            if matches!(tool, "task" | "Task" | "Agent" | "spawn_agent") {
                t.spawns += 1;
            }
        }
        st.pending.insert(call_id.to_string(), (ts, key));
        Ok(())
    }

    fn tool_result(&self, st: &mut FileState, call_id: &str, ts: i64, is_error: bool) -> Res<()> {
        if let Some((t0, key)) = st.pending.remove(call_id) {
            self.db
                .conn
                .execute("UPDATE tool_calls SET duration_ms = ?1, is_error = ?2 WHERE rec_key = ?3", params![(ts - t0).max(0), is_error as i64, key])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn ask_open(&self, st: &mut FileState, file: &str, call_id: &str, ts: i64, q: &Value, multi_key: &str) -> Res<()> {
        let first = q["questions"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        self.db
            .conn
            .execute(
                "INSERT INTO asks(rec_key, session_id, asked_at, question, options, multi) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(rec_key) DO NOTHING",
                params![
                    format!("{file}#ask:{call_id}"),
                    st.session_id,
                    ts,
                    first["question"].as_str(),
                    first["options"].to_string(),
                    first[multi_key].as_bool().unwrap_or(false) as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        st.ask = Some(call_id.to_string());
        Ok(())
    }

    fn ask_close(&self, st: &mut FileState, file: &str, call_id: &str, ts: i64, answer: &Value, cancelled: bool) -> Res<()> {
        if st.ask.as_deref() != Some(call_id) {
            return Ok(());
        }
        st.ask = None;
        self.db
            .conn
            .execute(
                "UPDATE asks SET answered_at = ?1, answer = ?2, cancelled = ?3,
                   channel = CASE WHEN EXISTS (SELECT 1 FROM human_messages WHERE rec_key = ?5) THEN 'dashboard' ELSE COALESCE(channel, 'terminal') END
                 WHERE rec_key = ?4",
                params![ts, answer.to_string(), cancelled as i64, format!("{file}#ask:{call_id}"), format!("{file}#answer:{call_id}")],
            )
            .map_err(|e| e.to_string())?;
        // the answer is my message too: dashboard answers already have a row (text = the
        // readable answer from now on), answers typed in the terminal get one
        let items = crate::ask::answer_items(answer);
        if cancelled || items.is_empty() {
            return Ok(());
        }
        let text = crate::ask::answer_text(&items);
        let key = format!("{file}#answer:{call_id}");
        let updated = self
            .db
            .conn
            .execute(
                "UPDATE human_messages SET text = ?2, chars = ?3, lang = ?4 WHERE rec_key = ?1",
                params![key, text, text.chars().count() as i64, lang_guess(&text)],
            )
            .map_err(|e| e.to_string())?;
        if updated == 0 {
            let (project, _) = self.session_info(st.session_id);
            let n = self
                .db
                .conn
                .execute(
                    "INSERT INTO human_messages(rec_key, session_id, ts, project, agent_name, agent_type, model, kind, channel, chars, lines, lang, attachments, is_slash, steering, text, sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5, (SELECT agent_type FROM sessions WHERE id = ?2), ?6, 'human', 'terminal-answer', ?7, ?8, ?9, 0, 0, 0, ?10, ?11) ON CONFLICT(rec_key) DO NOTHING",
                    params![key, st.session_id, ts, project, st.code, st.model, text.chars().count() as i64, text.lines().count().max(1) as i64, lang_guess(&text), text, format!("{:x}", Sha256::digest(text.as_bytes()))],
                )
                .map_err(|e| e.to_string())?;
            if n > 0 {
                self.index_text(&key, st.session_id, ts, "user", &text)?;
            }
        }
        Ok(())
    }

    fn record(&self, m: &SessionMeta, kind: SessionMetaKind, v: &Value, off: u64, st: &mut FileState) -> Res<()> {
        let file = m.file.as_str();
        let ts = ts_of(&v["timestamp"]);
        if let Some(t) = ts {
            st.first_ts = Some(st.first_ts.map_or(t, |f: i64| f.min(t)));
            st.last_ts = Some(st.last_ts.map_or(t, |l: i64| l.max(t)));
        }
        let ts = ts.unwrap_or(0);
        match kind {
            SessionMetaKind::Omp | SessionMetaKind::OmpSub => match v["type"].as_str().unwrap_or("") {
                "session" => self.set_cwd(st.session_id, v["cwd"].as_str().unwrap_or(""), v["id"].as_str())?,
                "title" | "title_change" => {
                    if let Some(t) = v["title"].as_str().filter(|t| !t.trim().is_empty()) {
                        let clean = crate::session::clean_title(t);
                        st.code = codename(t).or(st.code.take());
                        if !st.titles.contains(&clean) {
                            st.titles.push(clean);
                        }
                    }
                }
                "model_change" => {
                    if let Some(x) = v["model"].as_str() {
                        st.model = Some(x.to_string());
                        if !st.models.iter().any(|y| y == x) {
                            st.models.push(x.to_string());
                        }
                    }
                }
                "model_usage" => {
                    let u = &v["usage"];
                    self.usage(
                        st,
                        format!("{file}#{}", v["id"].as_str().unwrap_or(&off.to_string())),
                        ts,
                        v["purpose"].as_str().unwrap_or("aux"),
                        v["provider"].as_str(),
                        v["model"].as_str(),
                        u["input"].as_i64().unwrap_or(0),
                        u["output"].as_i64().unwrap_or(0),
                        u["cacheRead"].as_i64().unwrap_or(0),
                        u["cacheWrite"].as_i64().unwrap_or(0),
                        u["cost"]["total"].as_f64().unwrap_or(0.0),
                        false,
                    )?;
                }
                // a skill invocation is the user's message (typed text in details.prompt)
                "custom_message" => {
                    if let Some((text, _)) = crate::transcript::omp_user(v) {
                        let id = v["id"].as_str().map(String::from).unwrap_or_else(|| off.to_string());
                        self.user_message(m, st, format!("{file}#{id}"), ts, &text, 0)?;
                    }
                }
                "message" => {
                    let msg = &v["message"];
                    let id = v["id"].as_str().map(String::from).unwrap_or_else(|| off.to_string());
                    match msg["role"].as_str().unwrap_or("") {
                        "user" => {
                            let text = text_parts(&msg["content"]);
                            let images = msg["content"].as_array().map(|a| a.iter().filter(|c| c["type"] == "image").count() as i64).unwrap_or(0);
                            self.user_message(m, st, format!("{file}#{id}"), ts, &text, images)?;
                        }
                        // "!cmd" / "$ code" typed into omp: the user's input, run locally
                        "bashExecution" | "pythonExecution" => {
                            let p = if msg["role"] == "bashExecution" { "!" } else { "$ " };
                            let text = format!("{p}{}", msg["command"].as_str().or(msg["code"].as_str()).unwrap_or(""));
                            self.user_message(m, st, format!("{file}#{id}"), ts, &text, 0)?;
                        }
                        "assistant" => {
                            if let Some(x) = msg["model"].as_str() {
                                st.model = Some(x.to_string());
                                if !st.models.iter().any(|y| y == x) {
                                    st.models.push(x.to_string());
                                }
                            }
                            self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                            let chars: i64 = msg["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|c| c["type"] == "text")
                                .map(|c| c["text"].as_str().unwrap_or("").chars().count() as i64)
                                .sum();
                            self.assistant_text(st, format!("{file}#{id}"), ts, &text_parts(&msg["content"]))?;
                            if let Some(t) = st.turn.as_mut() {
                                t.chars += chars;
                                if t.model.is_none() {
                                    t.model = st.model.clone();
                                }
                            }
                            let u = &msg["usage"];
                            if u.is_object() {
                                self.usage(
                                    st,
                                    format!("{file}#{id}"),
                                    ts,
                                    "turn",
                                    msg["provider"].as_str(),
                                    msg["model"].as_str(),
                                    u["input"].as_i64().unwrap_or(0),
                                    u["output"].as_i64().unwrap_or(0),
                                    u["cacheRead"].as_i64().unwrap_or(0),
                                    u["cacheWrite"].as_i64().unwrap_or(0),
                                    u["cost"]["total"].as_f64().unwrap_or(0.0),
                                    true,
                                )?;
                            }
                            for c in msg["content"].as_array().into_iter().flatten() {
                                if c["type"] == "toolCall" {
                                    let cid = c["id"].as_str().unwrap_or("");
                                    let name = c["name"].as_str().unwrap_or("tool");
                                    self.tool_call(st, file, cid, ts, name, &c["arguments"])?;
                                    if name == "bash" {
                                        self.shell_send(m, st, file, cid, ts, &c["arguments"])?;
                                    }
                                    if name == "ask" {
                                        self.ask_open(st, file, cid, ts, &c["arguments"], "multi")?;
                                    }
                                }
                            }
                            match msg["stopReason"].as_str() {
                                Some("toolUse") | None => st.busy = true,
                                Some(r) => {
                                    st.busy = false;
                                    if matches!(r, "aborted" | "error") {
                                        if let Some(t) = st.turn.as_mut() {
                                            t.interrupted = true;
                                        }
                                    }
                                }
                            }
                        }
                        "toolResult" => {
                            self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                            let cid = msg["toolCallId"].as_str().unwrap_or("");
                            let err = msg["isError"] == true;
                            self.tool_result(st, cid, ts, err)?;
                            if msg["toolName"] == "ask" {
                                self.ask_close(st, file, cid, ts, &msg["details"], err)?;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            SessionMetaKind::Claude => {
                let id = v["uuid"].as_str().map(String::from).unwrap_or_else(|| off.to_string());
                if st.first_ts == Some(ts) {
                    if let Some(cwd) = v["cwd"].as_str() {
                        self.set_cwd(st.session_id, cwd, v["sessionId"].as_str())?;
                    }
                }
                let msg = &v["message"];
                match v["type"].as_str() {
                    Some("user") => {
                        if self.session_info(st.session_id).1.is_none() {
                            if let Some(cwd) = v["cwd"].as_str() {
                                self.set_cwd(st.session_id, cwd, v["sessionId"].as_str())?;
                            }
                        }
                        let content = &msg["content"];
                        let results: Vec<&Value> = content.as_array().map(|a| a.iter().filter(|c| c["type"] == "tool_result").collect()).unwrap_or_default();
                        if !results.is_empty() {
                            self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                            for c in results {
                                let cid = c["tool_use_id"].as_str().unwrap_or("");
                                let err = c["is_error"] == true;
                                self.tool_result(st, cid, ts, err)?;
                                self.ask_close(st, file, cid, ts, &c["content"], err)?;
                            }
                        } else if v["isMeta"] != true {
                            // slash commands are stored as <command-name> tags: keep what was typed
                            let raw = text_parts(content);
                            let text = crate::transcript::claude_command(&raw).map(|c| c.0).unwrap_or(raw);
                            if !text.trim().is_empty() {
                                self.user_message(m, st, format!("{file}#{id}"), ts, &text, 0)?;
                            }
                        }
                    }
                    Some("assistant") => {
                        if let Some(x) = msg["model"].as_str() {
                            st.model = Some(x.to_string());
                            if !st.models.iter().any(|y| y == x) {
                                st.models.push(x.to_string());
                            }
                        }
                        self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                        let u = &msg["usage"];
                        if u.is_object() {
                            let mid = msg["id"].as_str().map(String::from).unwrap_or(id.clone());
                            self.usage(
                                st,
                                format!("{file}#{mid}"),
                                ts,
                                "turn",
                                Some("anthropic"),
                                msg["model"].as_str(),
                                u["input_tokens"].as_i64().unwrap_or(0),
                                u["output_tokens"].as_i64().unwrap_or(0),
                                u["cache_read_input_tokens"].as_i64().unwrap_or(0),
                                u["cache_creation_input_tokens"].as_i64().unwrap_or(0),
                                0.0,
                                true,
                            )?;
                        }
                        for c in msg["content"].as_array().into_iter().flatten() {
                            match c["type"].as_str() {
                                Some("text") => {
                                    if let Some(t) = st.turn.as_mut() {
                                        t.chars += c["text"].as_str().unwrap_or("").chars().count() as i64;
                                    }
                                    self.assistant_text(st, format!("{file}#{id}"), ts, c["text"].as_str().unwrap_or(""))?;
                                }
                                Some("tool_use") => {
                                    let cid = c["id"].as_str().unwrap_or("");
                                    let name = c["name"].as_str().unwrap_or("tool");
                                    self.tool_call(st, file, cid, ts, name, &c["input"])?;
                                    if name == "Bash" {
                                        self.shell_send(m, st, file, cid, ts, &c["input"])?;
                                    }
                                    if name == "AskUserQuestion" {
                                        self.ask_open(st, file, cid, ts, &c["input"], "multiSelect")?;
                                    }
                                }
                                _ => {}
                            }
                        }
                        st.busy = msg["stop_reason"].as_str().is_none_or(|r| r == "tool_use");
                    }
                    _ => {}
                }
            }
            SessionMetaKind::Codex => {
                let p = &v["payload"];
                match (v["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or("")) {
                    ("session_meta", _) => self.set_cwd(st.session_id, p["cwd"].as_str().unwrap_or(""), p["id"].as_str())?,
                    ("turn_context", _) => {
                        if let Some(x) = p["model"].as_str() {
                            st.model = Some(x.to_string());
                            if !st.models.iter().any(|y| y == x) {
                                st.models.push(x.to_string());
                            }
                        }
                    }
                    ("event_msg", "task_started") => {
                        if st.origin.is_none() {
                            st.origin = p["turn_attribution"]["turn_trigger"].as_str().map(String::from);
                        }
                    }
                    ("event_msg", "user_message") => {
                        let text = p["message"].as_str().unwrap_or("").to_string();
                        let images =
                            p["images"].as_array().map(|a| a.len() as i64).unwrap_or(0) + p["local_images"].as_array().map(|a| a.len() as i64).unwrap_or(0);
                        let key = format!("{file}#{off}");
                        self.user_message(m, st, key, ts, &text, images)?;
                        if st.origin.as_deref() == Some("exec") {
                            // programmatic `codex exec` prompt, not typed by the human
                            self.db
                                .conn
                                .execute("UPDATE human_messages SET kind = 'exec', channel = 'exec' WHERE rec_key = ?1", [format!("{file}#{off}")])
                                .map_err(|e| e.to_string())?;
                            if let Some(t) = st.turn.as_mut() {
                                t.trigger = "exec".into();
                            }
                        }
                    }
                    ("event_msg", "token_count") => {
                        let u = &p["info"]["last_token_usage"];
                        let total = p["info"]["total_token_usage"]["total_tokens"].as_i64();
                        // count a usage record only when the running total moved
                        let fresh = total.is_none() || total != st.codex_total;
                        if total.is_some() {
                            st.codex_total = total;
                        }
                        if u.is_object() && fresh {
                            let cached = u["cached_input_tokens"].as_i64().unwrap_or(0);
                            self.usage(
                                st,
                                format!("{file}#{off}"),
                                ts,
                                "turn",
                                Some("openai"),
                                st.model.clone().as_deref(),
                                u["input_tokens"].as_i64().unwrap_or(0) - cached,
                                u["output_tokens"].as_i64().unwrap_or(0),
                                cached,
                                u["cache_write_input_tokens"].as_i64().unwrap_or(0),
                                0.0,
                                true,
                            )?;
                        }
                    }
                    ("event_msg", "task_complete") => {
                        self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                        st.busy = false;
                    }
                    ("event_msg", "turn_aborted") => {
                        if let Some(t) = st.turn.as_mut() {
                            t.interrupted = true;
                        }
                        st.busy = false;
                    }
                    ("response_item", "message") if p["role"] == "assistant" => {
                        self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                        let chars: i64 = p["content"].as_array().into_iter().flatten().map(|c| c["text"].as_str().unwrap_or("").chars().count() as i64).sum();
                        if let Some(t) = st.turn.as_mut() {
                            t.chars += chars;
                        }
                        let text: String = p["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
                        let id = p["id"].as_str().map(String::from).unwrap_or_else(|| format!("c{off}"));
                        self.assistant_text(st, format!("{file}#{id}"), ts, &text)?;
                    }
                    ("response_item", "function_call" | "custom_tool_call") => {
                        self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                        let cid = p["call_id"].as_str().unwrap_or("");
                        let args = match &p["arguments"] {
                            Value::String(a) => serde_json::from_str::<Value>(a).unwrap_or(Value::Null),
                            Value::Null => p["input"].clone(),
                            v => v.clone(),
                        };
                        self.tool_call(st, file, cid, ts, p["name"].as_str().unwrap_or("tool"), &args)?;
                        if matches!(p["name"].as_str(), Some("shell" | "exec_command" | "local_shell" | "shell_command")) {
                            self.shell_send(m, st, file, cid, ts, if p["arguments"].is_null() { &p["input"] } else { &p["arguments"] })?;
                        }
                    }
                    ("response_item", "function_call_output" | "custom_tool_call_output") => {
                        self.activity(m, st, ts, &format!("{file}#auto{off}"))?;
                        let cid = p["call_id"].as_str().unwrap_or("");
                        self.tool_result(st, cid, ts, false)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum SessionMetaKind {
    Omp,
    OmpSub,
    Claude,
    Codex,
}

impl SessionMetaKind {
    fn agent_type(self) -> &'static str {
        match self {
            Self::Omp | Self::OmpSub => "omp",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// conversationId → workspace (cwd) from agy's prompt history.
fn agy_workspaces() -> HashMap<String, String> {
    let text = std::fs::read_to_string(crate::agy::root().join("history.jsonl")).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| Some((v["conversationId"].as_str()?.to_string(), v["workspace"].as_str()?.to_string())))
        .collect()
}

/// Cheap byte pre-filter: skip lines that can't carry a fact (saves parsing GBs of tool output).
fn relevant(kind: SessionMetaKind, line: &[u8]) -> bool {
    let has = |n: &[u8]| line.windows(n.len()).any(|w| w == n);
    match kind {
        SessionMetaKind::Omp | SessionMetaKind::OmpSub => line.starts_with(b"{\"type\":\"") || has(b"\"type\":\"message\"") || has(b"\"type\": \"message\""),
        SessionMetaKind::Claude => true,
        SessionMetaKind::Codex => !has(b"\"type\":\"reasoning\"") && !has(b"\"world_state\"") && !has(b"\"item_completed\""),
    }
}

fn text_parts(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn walk(root: &Path, f: &mut dyn FnMut(&Path)) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "jsonl") => f(&p),
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// other sources: history.jsonl transitions, threads, dashboard events, snapshots

impl Db {
    /// One provider error from a session record (no-op for other records).
    pub fn error_row(&self, agent_type: &str, file: &str, session_id: i64, v: &Value, off: u64, model: Option<&str>) -> Res<()> {
        let ts = v["timestamp"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp_millis())
            .or_else(|| v["timestamp"].as_i64())
            .unwrap_or(0);
        let Some(e) = crate::session::record_error(agent_type, v, ts) else { return Ok(()) };
        // omp writes how an auto-retry ended on the error record itself
        let recovered = v["message"]["retryRecovery"]["status"] == "recovered";
        self.conn
            .execute(
                "INSERT INTO errors(rec_key, session_id, agent_type, ts, provider, model, status, message, recovered) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(rec_key) DO UPDATE SET recovered = ?9, status = ?7, message = ?8",
                params![
                    format!("{file}#{off}"),
                    session_id,
                    agent_type,
                    ts,
                    e.provider,
                    e.model.as_deref().or(model).map(norm_model),
                    e.status,
                    e.message,
                    recovered
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Errors in files read before the errors table existed: scan only lines that can be one.
    pub fn backfill_errors(&self) -> Res<usize> {
        let files: Vec<(String, String, i64)> = {
            let mut st = self
                .conn
                .prepare("SELECT s.file, s.agent_type, s.id FROM sessions s WHERE s.agent_type IN ('omp','claude','codex')")
                .map_err(|e| e.to_string())?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
            v
        };
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let before: i64 = self.conn.query_row("SELECT COUNT(*) FROM errors", [], |r| r.get(0)).unwrap_or(0);
        for (file, kind, sid) in files {
            let Ok(f) = std::fs::File::open(&file) else { continue };
            let mut r = BufReader::with_capacity(1 << 20, f);
            let (mut off, mut line) = (0u64, Vec::new());
            loop {
                line.clear();
                let n = r.read_until(b'\n', &mut line).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let has = |p: &[u8]| line.windows(p.len()).any(|w| w == p);
                if has(b"\"stopReason\":\"error\"") || has(b"\"isApiErrorMessage\":true") || (kind == "codex" && has(b"\"type\":\"error\"")) {
                    if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                        self.error_row(&kind, &file, sid, &v, off, None)?;
                    }
                }
                off += n as u64;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        let after: i64 = self.conn.query_row("SELECT COUNT(*) FROM errors", [], |r| r.get(0)).unwrap_or(0);
        Ok((after - before) as usize)
    }

    /// State transitions from history.jsonl (all of it; UNIQUE makes repeats no-ops).
    pub fn ingest_history(&self) -> Res<usize> {
        // skip when history.jsonl has not changed since the last pass
        static SEEN: std::sync::Mutex<(i64, u64)> = std::sync::Mutex::new((0, 0));
        let hp = crate::util::state_dir().join("history.jsonl");
        let sig = (crate::util::mtime_ms(&hp), std::fs::metadata(&hp).map(|m| m.len()).unwrap_or(0));
        {
            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            if *seen == sig {
                return Ok(0);
            }
            *seen = sig;
        }
        let text = std::fs::read_to_string(&hp).unwrap_or_default();
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut n = 0;
        for l in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
            if v["kind"] == "send" || v["state"].is_null() {
                continue;
            }
            n += self
                .conn
                .execute(
                    "INSERT INTO state_transitions(identity, ts, state, prev_state, name, project) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
                    params![v["identity"].as_str(), v["ts"].as_i64(), v["state"].as_str(), v["prevState"].as_str(), v["name"].as_str(), v["project"].as_str()],
                )
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(n)
    }

    pub fn ingest_threads(&self) -> Res<usize> {
        let mut n = 0;
        for t in crate::threads::load_all() {
            let sends: Vec<i64> = t.events.iter().filter(|e| e.kind == "sent").map(|e| e.at).collect();
            let answered = t.events.iter().find(|e| e.kind == "answered").map(|e| e.at);
            n += self
                .conn
                .execute(
                    "INSERT INTO relay_threads(id, from_agent, to_agent, created_at, delivered_at, answered_at, relayed_at, hops, status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(id) DO UPDATE SET delivered_at=?5, answered_at=?6, relayed_at=?7, hops=?8, status=?9",
                    params![t.id, t.from, t.to, t.created_at, sends.first(), answered, if t.status == "relayed" { sends.get(1).copied() } else { None }, t.hops, t.status],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(n)
    }

    /// Sessions of agents launched as tests (handles in `test_handles.txt`) get `is_test = 1`,
    /// and every record they produced is removed.
    pub fn mark_test_sessions(&self, snap: &crate::collect::Snapshot) {
        let handles: std::collections::HashSet<String> = std::fs::read_to_string(data_dir().join("test_handles.txt"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        for a in snap.agents.iter().filter(|a| a.handle.as_ref().is_some_and(|h| handles.contains(h))) {
            if let Some(p) = &a.session_path {
                let _ = self.conn.execute("UPDATE sessions SET is_test = 1 WHERE file = ?1 AND is_test = 0", [p.to_string_lossy()]);
            }
            let _ = self.conn.execute("DELETE FROM state_transitions WHERE identity IN (?1, ?2)", [&a.identity, &a.pane_key]);
        }
        // closed ones too: any session that ran in a test terminal
        for h in &handles {
            let _ = self.conn.execute("UPDATE sessions SET is_test = 1 WHERE is_test = 0 AND EXISTS (SELECT 1 FROM json_each(handles) WHERE value = ?1)", [h]);
        }
        // throwaway sessions leave no records: their rows go (the session row stays, marked, so
        // the file is not ingested again)
        for t in ["human_messages", "agent_turns", "tool_calls", "usage_events", "asks", "assistant_messages", "agent_sends", "errors", "messages_fts"] {
            let _ = self.conn.execute(&format!("DELETE FROM {t} WHERE session_id IN (SELECT id FROM sessions WHERE is_test = 1)"), []);
        }
        let _ = self.conn.execute("DELETE FROM state_transitions WHERE identity IN (SELECT file FROM sessions WHERE is_test = 1)", []);
    }

    /// Fill turns without a model (codex starts a turn before its turn_context names the model;
    /// claude/agy gaps) from the nearest turn of the same session, then the session's last
    /// model; then store every model name normalized (`norm_model`), originals in `model_raw`.
    pub fn normalize_models(&self) -> Res<usize> {
        let c = &self.conn;
        // nearest turn of the same session that has a model (row by row: the bundled SQLite does
        // not allow the outer row in a correlated ORDER BY)
        let missing: Vec<(i64, i64, i64)> = {
            let mut st = c.prepare("SELECT id, session_id, start_ts FROM agent_turns WHERE model IS NULL OR model = ''").map_err(|e| e.to_string())?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
            v
        };
        let mut n = 0;
        for (id, sid, ts) in missing {
            let m: Option<String> = c
                .query_row(
                    "SELECT model FROM agent_turns WHERE session_id = ?1 AND model IS NOT NULL AND model != '' ORDER BY abs(start_ts - ?2) LIMIT 1",
                    params![sid, ts],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(m) = m {
                n += c.execute("UPDATE agent_turns SET model = ?2 WHERE id = ?1", params![id, m]).map_err(|e| e.to_string())?;
            }
        }
        n += c
            .execute(
                "UPDATE agent_turns SET model = (SELECT json_extract(s.models, '$[#-1]') FROM sessions s WHERE s.id = agent_turns.session_id)
                 WHERE (model IS NULL OR model = '') AND (SELECT json_array_length(s.models) FROM sessions s WHERE s.id = agent_turns.session_id) > 0",
                [],
            )
            .map_err(|e| e.to_string())?;
        for t in ["agent_turns", "usage_events", "human_messages", "tool_calls"] {
            // only rows not normalized yet (model_raw NULL; partial index makes this cheap)
            let models: Vec<String> = {
                let mut st = c
                    .prepare(&format!("SELECT DISTINCT model FROM {t} WHERE model_raw IS NULL AND model IS NOT NULL AND model != ''"))
                    .map_err(|e| e.to_string())?;
                let v = st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
                v
            };
            for m in models {
                c.execute(&format!("UPDATE {t} SET model_raw = ?1, model = ?2 WHERE model_raw IS NULL AND model = ?1"), params![m, norm_model(&m)])
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(n)
    }

    /// Send-time record of an agent→agent message (`adash tell` run by an agent).
    pub fn record_agent_send(&self, ts: i64, from: &str, to: &str, channel: &str, text: &str) {
        let _ = self.conn.execute(
            "INSERT INTO agent_sends(rec_key, ts, from_agent, to_agent, channel, text) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(rec_key) DO NOTHING",
            params![format!("live:{ts}:{to}"), ts, from, to, channel, text],
        );
    }

    /// Re-classify user messages that agents sent: `human` rows matching an agent send (text
    /// start within the sender's command, sent 10 min before to 1 min after) → `agent` with the
    /// sender; relay lines get their `from`; review-script pointer lines get the requester from
    /// the registered thread. Returns rows changed. `since` limits the pass (0 = everything).
    pub fn link_agent_messages(&self, since: i64, markers: &HashMap<String, String>) -> Res<usize> {
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut n = 0;
        {
            let mut st = tx
                .prepare(
                    "SELECT m.rec_key,
                            COALESCE(s.from_agent,
                                     (SELECT t.agent_name FROM agent_turns t WHERE t.session_id = s.session_id AND t.agent_name IS NOT NULL ORDER BY t.start_ts DESC LIMIT 1),
                                     (SELECT json_extract(x.names, '$[#-1]') FROM sessions x WHERE x.id = s.session_id),
                                     (SELECT json_extract(x.titles, '$[#-1]') FROM sessions x WHERE x.id = s.session_id),
                                     (SELECT x.project FROM sessions x WHERE x.id = s.session_id)),
                            s.channel
                     FROM human_messages m JOIN agent_sends s
                       ON s.ts BETWEEN m.ts - 600000 AND m.ts + 60000
                      AND (s.session_id IS NULL OR s.session_id != m.session_id)
                      AND length(m.text) >= 12
                      AND instr(s.text, substr(m.text, 1, 40)) > 0
                     WHERE m.kind = 'human' AND m.ts >= ?1",
                )
                .map_err(|e| e.to_string())?;
            let rows: Vec<(String, Option<String>, String)> =
                st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
            for (k, from, ch) in rows {
                n += tx
                    .execute(
                        "UPDATE human_messages SET kind = 'agent', channel = ?2, from_agent = ?3 WHERE rec_key = ?1 AND kind = 'human'",
                        params![k, ch, from],
                    )
                    .map_err(|e| e.to_string())?;
                tx.execute("UPDATE agent_turns SET trigger_kind = 'agent' WHERE rec_key = ?1", [&k]).map_err(|e| e.to_string())?;
            }
            // relay lines: "[adash T… from X] …"
            let mut st =
                tx.prepare("SELECT rec_key, text FROM human_messages WHERE kind = 'relay' AND from_agent IS NULL AND ts >= ?1").map_err(|e| e.to_string())?;
            let rows: Vec<(String, String)> =
                st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
            for (k, t) in rows {
                if let Some(from) = relay_from(&t) {
                    n += tx.execute("UPDATE human_messages SET from_agent = ?2 WHERE rec_key = ?1", params![k, from]).map_err(|e| e.to_string())?;
                }
            }
            // review-script pointers: requester from the thread registered under the marker
            let mut st =
                tx.prepare("SELECT rec_key, text FROM human_messages WHERE kind = 'peer' AND from_agent IS NULL AND ts >= ?1").map_err(|e| e.to_string())?;
            let rows: Vec<(String, String)> =
                st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
            for (k, t) in rows {
                let marker = t.strip_prefix('[').and_then(|r| r.split_once(']')).map(|x| x.0.to_string());
                if let Some(from) = marker.and_then(|m| markers.get(&m)).filter(|f| f.as_str() != "unknown") {
                    n += tx.execute("UPDATE human_messages SET from_agent = ?2 WHERE rec_key = ?1", params![k, from]).map_err(|e| e.to_string())?;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(n)
    }

    pub fn log_action(&self, action: &str, target: &str, detail: &Value) {
        let _ = self.conn.execute(
            "INSERT INTO dashboard_actions(ts, action, target, detail) VALUES (?1,?2,?3,?4) ON CONFLICT DO NOTHING",
            params![now_ms(), action, target, detail.to_string()],
        );
    }

    /// Record notifications queued by `notify::send` (title "질문 대기 …" → kind ask).
    pub fn drain_notifications(&self) {
        let pending: Vec<_> = crate::notify::PENDING.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (ts, title, body, target) in pending {
            let kind = if title.starts_with("질문 대기") {
                "ask"
            } else if title.starts_with("adash") {
                "relay"
            } else {
                "transition"
            };
            let _ = self.conn.execute(
                "INSERT INTO notifications(ts, kind, title, body, target) VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
                params![ts, kind, title, body, target],
            );
        }
    }

    /// An ask answered from the dashboard: channel on the ask + a `dashboard-answer` human message.
    pub fn record_answer(&self, session_file: &str, call_id: &str, ts: i64, text: &str) {
        let _ = self.conn.execute("UPDATE asks SET channel = 'dashboard' WHERE rec_key = ?1", params![format!("{session_file}#ask:{call_id}")]);
        let _ = self.conn.execute("INSERT INTO sessions(file, agent_type) VALUES (?1, 'omp') ON CONFLICT(file) DO NOTHING", [session_file]);
        let _ = self.conn.execute(
            "INSERT INTO human_messages(rec_key, session_id, ts, project, agent_type, kind, channel, chars, lines, lang, attachments, is_slash, steering, text, sha256)
             SELECT ?1, id, ?2, project, agent_type, 'human', 'dashboard-answer', ?3, 1, ?4, 0, 0, 0, ?5, ?6 FROM sessions WHERE file = ?7
             ON CONFLICT(rec_key) DO NOTHING",
            params![format!("{session_file}#answer:{call_id}"), ts, text.chars().count() as i64, lang_guess(text), text, format!("{:x}", Sha256::digest(text.as_bytes())), session_file],
        );
    }

    pub fn snapshot(&self, snap: &crate::collect::Snapshot) -> Res<()> {
        let mut per: HashMap<&str, [i64; 3]> = HashMap::new();
        let (mut w, mut wait, mut ask, mut idle) = (0, 0, 0, 0);
        for a in &snap.agents {
            let e = per.entry(a.project.as_str()).or_default();
            match a.state.as_str() {
                "working" => {
                    w += 1;
                    e[0] += 1;
                }
                "asking" => {
                    ask += 1;
                    e[2] += 1;
                }
                _ if a.needs_human => {
                    wait += 1;
                    e[1] += 1;
                }
                _ => idle += 1,
            }
        }
        let per_json: Value =
            per.iter().map(|(k, v)| (k.to_string(), json!({"working": v[0], "waiting": v[1], "asking": v[2]}))).collect::<serde_json::Map<_, _>>().into();
        self.conn
            .execute(
                "INSERT INTO snapshots(ts, live, working, waiting, asking, idle, per_project) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT DO NOTHING",
                params![snap.at, snap.agents.len() as i64, w, wait, ask, idle, per_json.to_string()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Daily backup via the SQLite backup API; keep the last 14 days + the first of each month.
    /// One compact, compressed copy per day (`VACUUM INTO`, then zstd → `analytics-YYYYMMDD.db.zst`);
    /// kept: the last 7 days and the first of each month. Older plain `.db` copies are
    /// compressed on the way. Names that are not `analytics-<8 digits>` are left alone.
    pub fn daily_backup(&self) -> Res<Option<PathBuf>> {
        let dir = data_dir().join("backups");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let target = dir.join(format!("analytics-{today}.db.zst"));
        let made = if target.exists() || dir.join(format!("analytics-{today}.db")).exists() && !zstd_available() {
            None
        } else {
            let tmp = dir.join(format!(".analytics-{today}.db.tmp"));
            let _ = std::fs::remove_file(&tmp);
            self.conn.execute("VACUUM INTO ?1", [tmp.to_string_lossy()]).map_err(|e| format!("VACUUM INTO: {e}"))?;
            Some(compress(&tmp, &target)?)
        };
        let day_of = |name: &str| -> Option<String> {
            let d = name.strip_prefix("analytics-")?.strip_suffix(".db.zst").or_else(|| name.strip_prefix("analytics-")?.strip_suffix(".db"))?;
            (d.len() == 8 && d.chars().all(|c| c.is_ascii_digit())).then(|| d.to_string())
        };
        let files: Vec<(String, PathBuf)> =
            std::fs::read_dir(&dir).into_iter().flatten().flatten().filter_map(|e| day_of(&e.file_name().to_string_lossy()).map(|d| (d, e.path()))).collect();
        let mut days: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
        days.sort();
        days.dedup();
        let keep_recent: Vec<&String> = days.iter().rev().take(7).collect();
        let mut firsts: HashMap<&str, &String> = HashMap::new();
        for d in &days {
            firsts.entry(&d[..6]).or_insert(d);
        }
        for (d, p) in &files {
            let keep = keep_recent.contains(&d) || firsts.values().any(|f| *f == d);
            if !keep {
                let _ = std::fs::remove_file(p);
            } else if p.extension().is_some_and(|x| x == "db") && zstd_available() {
                // older full copies: compress in place
                let _ = compress(p, &p.with_extension("db.zst"));
            }
        }
        Ok(made)
    }
}

/// Ingestion context from current adash state (meta.yaml projects, terminal directories, live
/// names) + logged sends.
pub fn context(snap: Option<&crate::collect::Snapshot>) -> Ctx {
    let mut ctx = Ctx::default();
    let meta = crate::meta::Meta::load();
    for (name, pm) in &meta.projects {
        for p in &pm.paths {
            ctx.projects.push((p.clone(), name.clone()));
        }
    }
    if let Some(s) = snap {
        ctx.projects.extend(s.worktrees.iter().map(|w| (w.path.clone(), w.project.clone())));
        for a in &s.agents {
            if let (Some(p), Some(h)) = (&a.session_path, &a.handle) {
                if a.mapping_confidence != "heuristic" {
                    ctx.live.insert(p.clone(), (a.name.clone(), h.clone(), a.mapping_confidence.to_string()));
                }
            }
        }
    }
    let hist = std::fs::read_to_string(crate::util::state_dir().join("history.jsonl")).unwrap_or_default();
    for l in hist.lines().filter(|l| l.contains("\"kind\":\"send\"")) {
        if let Ok(v) = serde_json::from_str::<Value>(l) {
            ctx.sends.push(Send {
                ts: v["ts"].as_i64().unwrap_or(0),
                text40: v["text"].as_str().unwrap_or("").chars().take(40).collect(),
                channel: v["channel"].as_str().unwrap_or("dashboard-chat").to_string(),
            });
        }
    }
    ctx
}

#[derive(Serialize, Debug, Default)]
pub struct Totals {
    pub sessions: i64,
    pub subagent_sessions: i64,
    pub human_messages: i64,
    pub all_user_messages: i64,
    pub turns: i64,
    pub tool_calls: i64,
    pub usage_events: i64,
    pub asks: i64,
    pub transitions: i64,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
}

impl Db {
    pub fn totals(&self) -> Totals {
        let q = |sql: &str| -> i64 { self.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        Totals {
            sessions: q("SELECT COUNT(*) FROM sessions"),
            subagent_sessions: q("SELECT COUNT(*) FROM sessions WHERE is_subagent = 1"),
            human_messages: q("SELECT COUNT(*) FROM human_messages WHERE kind = 'human'"),
            all_user_messages: q("SELECT COUNT(*) FROM human_messages"),
            turns: q("SELECT COUNT(*) FROM agent_turns"),
            tool_calls: q("SELECT COUNT(*) FROM tool_calls"),
            usage_events: q("SELECT COUNT(*) FROM usage_events"),
            asks: q("SELECT COUNT(*) FROM asks"),
            transitions: q("SELECT COUNT(*) FROM state_transitions"),
            first_ts: self.conn.query_row("SELECT MIN(ts) FROM human_messages WHERE ts > 0", [], |r| r.get(0)).ok().flatten(),
            last_ts: self.conn.query_row("SELECT MAX(ts) FROM human_messages", [], |r| r.get(0)).ok().flatten(),
        }
    }
}

/// Full incremental ingest of every source. Used by `adash analytics backfill` and the server.
pub fn ingest_all(db: &Db, ctx: &Ctx) -> Res<Value> {
    let t0 = std::time::Instant::now();
    let files = Ingest::new(db, ctx).all_sessions()?;
    let transitions = db.ingest_history()?;
    let threads = db.ingest_threads()?;
    // agent→agent re-classification: the last day each run (senders' records can arrive after
    // the receiver's message); everything when the replay after a migration ran
    let markers: HashMap<String, String> = crate::threads::load_all().into_iter().filter_map(|t| t.marker.clone().map(|m| (m, t.from.clone()))).collect();
    let since = if files.files > 200 { 0 } else { now_ms() - 86_400_000 };
    let linked = db.link_agent_messages(since, &markers)?;
    db.normalize_models()?;
    Ok(
        json!({"filesChanged": files.files, "bytesRead": files.bytes, "newTransitions": transitions, "threads": threads, "agentMessagesLinked": linked, "seconds": t0.elapsed().as_secs_f64()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("adash-an-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn omp_fixture(dir: &Path) -> PathBuf {
        let f = dir.join("2026-10-29T00-00-00-000Z_s1.jsonl");
        let lines = [
            r#"{"type":"title","title":"\udb80\udd4d PLANNER: Draft proposal"}"#,
            r#"{"type":"session","id":"s1","timestamp":"2026-10-29T00:00:00.000Z","cwd":"/w/galaxy-survey"}"#,
            r#"{"type":"model_change","timestamp":"2026-10-29T00:00:00.100Z","model":"anthropic/claude-opus-5-5"}"#,
            r#"{"type":"message","id":"u1","timestamp":"2026-10-29T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"제안서 초안을 써 주세요"}]}}"#,
            r#"{"type":"message","id":"a1","timestamp":"2026-10-29T00:00:05.000Z","message":{"role":"assistant","model":"claude-opus-5-5","content":[{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"x.md"}}],"usage":{"input":10,"output":20,"cacheRead":100,"cacheWrite":5,"cost":{"total":0.5}},"stopReason":"toolUse"}}"#,
            r#"{"type":"message","id":"r1","timestamp":"2026-10-29T00:00:06.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"read","content":[],"isError":false}}"#,
            r#"{"type":"message","id":"s9","timestamp":"2026-10-29T00:00:07.000Z","message":{"role":"user","content":[{"type":"text","text":"영어로 써 주세요"}]}}"#,
            r#"{"type":"message","id":"a2","timestamp":"2026-10-29T00:00:30.000Z","message":{"role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"Done."}],"usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.1}},"stopReason":"stop"}}"#,
            r#"{"type":"message","id":"u2","timestamp":"2026-10-29T00:10:30.000Z","message":{"role":"user","content":[{"type":"text","text":"/compact"}]}}"#,
            r#"{"type":"message","id":"u3","timestamp":"2026-10-29T00:10:31.000Z","message":{"role":"user","content":[{"type":"text","text":"[adash T1 from UI] 검토 요청: /x/request.md 를 읽고 지시대로 답해 주세요."}]}}"#,
        ];
        std::fs::write(&f, lines.join("\n") + "\n").unwrap();
        f
    }

    fn counts(db: &Db) -> (i64, i64, i64, i64, i64) {
        let q = |s: &str| -> i64 { db.conn.query_row(s, [], |r| r.get(0)).unwrap() };
        (
            q("SELECT COUNT(*) FROM sessions"),
            q("SELECT COUNT(*) FROM human_messages"),
            q("SELECT COUNT(*) FROM agent_turns"),
            q("SELECT COUNT(*) FROM tool_calls"),
            q("SELECT COUNT(*) FROM usage_events"),
        )
    }

    #[test]
    fn migrates_from_empty_and_reopens() {
        let d = tmpdir("mig");
        let p = d.join("a.db");
        let db = open_at(&p).unwrap();
        let v: i64 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        drop(db);
        assert!(open_at(&p).is_ok(), "second open must not re-run v1 DDL");
    }

    #[test]
    fn ingest_is_idempotent_even_from_offset_zero() {
        let d = tmpdir("idem");
        let f = omp_fixture(&d);
        let db = open_at(&d.join("a.db")).unwrap();
        let ctx = Ctx::default();
        let ing = Ingest::new(&db, &ctx);
        ing.file(&f, SessionMetaKind::Omp).unwrap();
        let first = counts(&db);
        assert_eq!(first, (1, 4, 3, 1, 2));
        ing.file(&f, SessionMetaKind::Omp).unwrap(); // unchanged: skipped
                                                     // forget offsets entirely → full replay must not double count
        db.conn.execute("DELETE FROM ingest_files", []).unwrap();
        ing.file(&f, SessionMetaKind::Omp).unwrap();
        assert_eq!(counts(&db), first);
        // facts derived from the fixture
        let (steer, kinds): (i64, String) = db
            .conn
            .query_row("SELECT SUM(steering), GROUP_CONCAT(kind || ':' || channel || ':' || is_slash, ',') FROM human_messages", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(steer, 1, "'영어로 써 주세요' arrived while the agent was mid-turn");
        assert_eq!(kinds, "human:terminal:0,human:terminal:0,human:terminal:1,relay:relay:0");
        let (dur, gap, cost, tools): (i64, Option<i64>, f64, i64) = db
            .conn
            .query_row("SELECT duration_ms, human_gap_ms, cost, tool_calls FROM agent_turns ORDER BY start_ts LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!((dur, gap, tools), (29_000, None, 1));
        assert!((cost - 0.6).abs() < 1e-9);
        let gap2: i64 = db.conn.query_row("SELECT human_gap_ms FROM agent_turns WHERE rec_key LIKE '%#u2'", [], |r| r.get(0)).unwrap();
        assert_eq!(gap2, 600_000, "/compact came 10 min after the turn ended");
        let name: String = db.conn.query_row("SELECT agent_name FROM human_messages LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "PLANNER");
    }

    /// omp records a skill invocation as custom_message/skill-prompt; it is the user's message
    /// (typed text from details.prompt, not the skill body), and an agent-injected one is not.
    #[test]
    fn omp_skill_invocations_count_as_user_messages() {
        let d = tmpdir("skill");
        let f = d.join("2026-10-30T00-00-00-000Z_s2.jsonl");
        let lines = [
            r#"{"type":"session","id":"s2","timestamp":"2026-10-30T06:00:00.000Z","cwd":"/w/RS"}"#,
            r#"{"type":"custom_message","customType":"skill-prompt","display":true,"attribution":"user","id":"k1","timestamp":"2026-10-30T06:19:08.052Z","content":"[IMPORTANT: User invoked the \"magi-research\" skill] # magi-research body …","details":{"name":"magi-research","path":"/s/magi-research/SKILL.md","args":"음 약한 선도 좀 애매해. 에게 제안받자","prompt":"음 약한 선도 좀 애매해. /skill:magi-research 에게 제안받자"}}"#,
            r#"{"type":"custom_message","customType":"skill-prompt","display":true,"attribution":"agent","id":"k2","timestamp":"2026-10-30T06:20:00.000Z","content":"body","details":{"name":"x","prompt":"agent-loaded"}}"#,
        ];
        std::fs::write(&f, lines.join("\n") + "\n").unwrap();
        let db = open_at(&d.join("a.db")).unwrap();
        let ctx = Ctx::default();
        Ingest::new(&db, &ctx).file(&f, SessionMetaKind::Omp).unwrap();
        let rows: Vec<(String, String)> = {
            let mut st = db.conn.prepare("SELECT kind, text FROM human_messages").unwrap();
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().filter_map(Result::ok).collect();
            v
        };
        assert_eq!(rows, vec![("human".to_string(), "음 약한 선도 좀 애매해. /skill:magi-research 에게 제안받자".to_string())]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn appended_lines_are_ingested_incrementally() {
        let d = tmpdir("inc");
        let f = omp_fixture(&d);
        let db = open_at(&d.join("a.db")).unwrap();
        let ctx = Ctx::default();
        let ing = Ingest::new(&db, &ctx);
        ing.file(&f, SessionMetaKind::Omp).unwrap();
        let mut s = std::fs::read_to_string(&f).unwrap();
        s.push_str(
            r#"{"type":"message","id":"u4","timestamp":"2026-10-29T01:00:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Thanks"}]}}"#,
        );
        s.push('\n');
        std::fs::write(&f, s).unwrap();
        ing.file(&f, SessionMetaKind::Omp).unwrap();
        assert_eq!(counts(&db).1, 5);
    }

    #[test]
    fn slash_and_language_detection() {
        assert!(is_slash_command("/compact"));
        assert!(is_slash_command("  /model opus"));
        assert!(!is_slash_command("/home/alice/x.md 를 읽고 지시대로"));
        assert!(!is_slash_command("/tmp/a.png"));
        assert!(!is_slash_command("// comment"));
        assert_eq!(lang_guess("제안서 초안을 써 주세요"), "ko");
        assert_eq!(lang_guess("Read BRIEF.md and carry out the task"), "en");
        assert_eq!(lang_guess("BRIEF_v9 를 읽고 끝까지 수행해 주세요 analytics"), "mixed");
        assert_eq!(message_kind("[adash] 대시보드 이름 지정", false), "adash");
        assert_eq!(message_kind("[roadmap-v1 2026-10-15] @2027/strategy/_guide/brief.md 를 읽고 답해줘. 첫 줄에 '수락'", false), "peer");
        // the user's own bracketed notes stay human
        assert_eq!(message_kind("[메모] @foo.md 를 확인해 줘", false), "human");
        for (raw, want) in [
            ("anthropic/claude-opus-5-5", "claude-opus-5-5"),
            ("claude-opus-5-5", "claude-opus-5-5"),
            ("openai-codex/gpt-6-astra", "gpt-6-astra"),
            ("zai/glm-5.3", "glm-5.3"),
            ("anthropic/claude-opus-5-5:low", "claude-opus-5-5"),
            ("anthropic/claude-haiku-4-5-20251001", "claude-haiku-4-5"),
            ("gemini-4-argon-medium", "gemini-4-argon"),
            ("Gemini-4-Argon", "gemini-4-argon"),
            ("gpt-5.3-codex-high", "gpt-5.3-codex"),
        ] {
            assert_eq!(norm_model(raw), want, "{raw}");
        }
        assert_eq!(
            (
                model_family("claude-opus-5-5").as_str(),
                model_family("gpt-6-astra").as_str(),
                model_family("gemini-4-argon").as_str(),
                model_family("glm-5.3").as_str()
            ),
            ("opus", "gpt", "gemini", "glm")
        );
        let e = serde_json::json!({"i": "x", "input": "[src/term.rs#FFCE]\nPUT 5.=23:\n+use x;\n[README.md#3C4D]\nPUT >2:\n+- [ ] item"});
        assert_eq!(tool_target("edit", &e).as_deref(), Some("file:src/term.rs\nfile:README.md"));
        assert_eq!(tool_target("write", &serde_json::json!({"path": "agent://Main", "content": "x"})), None);
        assert_eq!(tool_target("read", &serde_json::json!({"path": "notes.txt:10-20"})).as_deref(), Some("file:notes.txt"));
        assert_eq!(tool_target("bash", &serde_json::json!({"command": "cd /x && python3 a.py"})).as_deref(), Some("cmd:python3"));
        assert_eq!(message_kind("[AITELL ASTRA2 v2] @_guide/b.md 를 읽고 지시대로 검토해줘. 답은 한 파일에만", false), "peer");
        assert_eq!(message_kind("[참고]\n@x.md 를 읽고 답해줘", false), "human");
        assert_eq!(message_kind("hi", true), "subagent");
    }

    #[test]
    fn every_write_stores_the_normalized_model() {
        let d = std::env::temp_dir().join(format!("adash-norm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let db = super::open_at(&d.join("a.db")).unwrap();
        let c = &db.conn;
        c.execute("INSERT INTO sessions(file, agent_type) VALUES ('f', 'omp')", []).unwrap();
        c.execute("INSERT INTO agent_turns(rec_key, session_id, trigger_kind, start_ts, end_ts, duration_ms, model) VALUES ('k', 1, 'human', 0, 1, 1, 'anthropic/claude-opus-5-5')", []).unwrap();
        // an upsert that writes the raw name again (the old leak)
        c.execute("INSERT INTO agent_turns(rec_key, session_id, trigger_kind, start_ts, end_ts, duration_ms, model) VALUES ('k', 1, 'human', 0, 2, 2, 'anthropic/claude-haiku-5-5:high') ON CONFLICT(rec_key) DO UPDATE SET model = excluded.model", []).unwrap();
        c.execute("INSERT INTO usage_events(rec_key, session_id, ts, purpose, model) VALUES ('u', 1, 0, 'turn', 'openai-codex/gpt-5.5-codex-high')", [])
            .unwrap();
        for t in ["agent_turns", "usage_events", "human_messages", "tool_calls"] {
            let bad: Vec<String> = {
                let mut st = c.prepare(&format!("SELECT DISTINCT model FROM {t} WHERE model IS NOT NULL AND model != ''")).unwrap();
                let v: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().filter_map(Result::ok).collect();
                v.into_iter().filter(|m| super::norm_model(m) != *m).collect()
            };
            assert!(bad.is_empty(), "{t}: unnormalized {bad:?}");
        }
        let (m, raw): (String, String) =
            c.query_row("SELECT model, model_raw FROM agent_turns WHERE rec_key = 'k'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((m.as_str(), raw.as_str()), ("claude-haiku-5-5", "anthropic/claude-haiku-5-5:high"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
