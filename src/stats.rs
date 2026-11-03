//! `adash stats` / `GET /api/stats`: aggregate the analytics store.
use crate::analytics::Db;
use crate::util::Res;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// SQL expression for the grouping key over a table whose time column is `ts_col`.
fn key_expr(by: &str, ts_col: &str, has_agent: bool) -> Res<String> {
    let local = format!("datetime({ts_col}/1000, 'unixepoch', 'localtime')");
    Ok(match by {
        "all" => "'all'".into(),
        "agent" => {
            if has_agent {
                "COALESCE(agent_name, '-')".into()
            } else {
                "'-'".into()
            }
        }
        "project" => "COALESCE(project, '-')".into(),
        "model" => "COALESCE(NULLIF(model, ''), '미상')".into(),
        "family" => "COALESCE(model_family(NULLIF(model, '')), '미상')".into(),
        "type" => "COALESCE(agent_type, '-')".into(),
        "day" => format!("date({local})"),
        "hour" => format!("strftime('%H', {local})"),
        "weekday" => format!("strftime('%w', {local})"),
        other => return Err(format!("unknown --by {other} (agent|project|model|family|type|day|hour|weekday)")),
    })
}

/// Local-date bounds → epoch ms [from, to).
pub fn bounds(from: Option<&str>, to: Option<&str>) -> Res<(i64, i64)> {
    let parse = |d: &str| -> Res<i64> {
        let nd = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|e| format!("bad date {d}: {e}"))?;
        let local = nd.and_hms_opt(0, 0, 0).unwrap().and_local_timezone(chrono::Local).single().ok_or("ambiguous local date")?;
        Ok(local.timestamp_millis())
    };
    let f = from.map(parse).transpose()?.unwrap_or(0);
    let t = match to {
        Some(d) => parse(d)? + 86_400_000, // inclusive end day
        None => i64::MAX,
    };
    Ok((f, t))
}

fn median(v: &mut [i64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] as f64 } else { (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0 })
}

fn round(x: f64, d: i32) -> f64 {
    let p = 10f64.powi(d);
    (x * p).round() / p
}

fn entry<'a>(groups: &'a mut BTreeMap<String, Value>, k: &str) -> &'a mut Value {
    groups.entry(k.to_string()).or_insert_with(|| {
        json!({"key": k, "messages": 0, "activeDays": 0, "turns": 0, "workingHours": 0.0, "medianAgentResponseSec": null,
               "medianHumanResponseSec": null, "humanResponsesAfterCacheTtl": null, "humanResponsesAfterCache60m": null,
               "topTools": [], "tokens": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "cost": 0.0})
    })
}

/// Provider/API errors in [from, to) by provider and status: count, auto-recovered, last seen.
pub fn errors(db: &Db, from: i64, to: i64) -> Res<Value> {
    let mut st = db
        .conn
        .prepare(
            "SELECT p, status, n, rec, last, (SELECT message FROM errors e2 WHERE e2.ts = g.last LIMIT 1)
             FROM (SELECT COALESCE(provider, '미상') AS p, status, COUNT(*) AS n, SUM(recovered) AS rec, MAX(ts) AS last
                   FROM errors WHERE ts >= ?1 AND ts < ?2 GROUP BY 1, 2) g ORDER BY n DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<Value> = st
        .query_map(rusqlite::params![from, to], |r| {
            Ok(json!({"provider": r.get::<_, String>(0)?, "status": r.get::<_, Option<i64>>(1)?, "count": r.get::<_, i64>(2)?, "recovered": r.get::<_, i64>(3)?, "lastAt": r.get::<_, i64>(4)?, "lastMessage": r.get::<_, Option<String>>(5)?}))
        })
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    Ok(json!({"from": from, "to": to, "by": "errors", "errors": rows}))
}

pub fn render_errors(v: &Value) -> String {
    let mut s = format!("{:<12} {:>6} {:>7} {:>9}  {}\n", "PROVIDER", "STATUS", "COUNT", "RECOVERED", "LAST MESSAGE");
    for r in v["errors"].as_array().into_iter().flatten() {
        s += &format!(
            "{:<12} {:>6} {:>7} {:>9}  {}\n",
            r["provider"].as_str().unwrap_or(""),
            r["status"].as_i64().map(|x| x.to_string()).unwrap_or("-".into()),
            r["count"].as_i64().unwrap_or(0),
            r["recovered"].as_i64().unwrap_or(0),
            crate::util::truncate(r["lastMessage"].as_str().unwrap_or(""), 70)
        );
    }
    s
}

pub fn stats(db: &Db, from: i64, to: i64, by: &str) -> Res<Value> {
    let c = &db.conn;
    let err = |e: rusqlite::Error| e.to_string();
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();

    // human messages and active days
    let k = key_expr(by, "ts", true)?;
    let sql = format!(
        "SELECT {k}, COUNT(*), COUNT(DISTINCT date(datetime(ts/1000,'unixepoch','localtime'))) FROM human_messages WHERE kind = 'human' AND ts >= ?1 AND ts < ?2 GROUP BY 1"
    );
    let mut st = c.prepare(&sql).map_err(err)?;
    for r in st.query_map([from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))).map_err(err)? {
        let (key, n, days) = r.map_err(err)?;
        let e = entry(&mut groups, &key);
        e["messages"] = json!(n);
        e["activeDays"] = json!(days);
    }

    // turns (human-triggered), working time, latencies
    let k = key_expr(by, "start_ts", true)?;
    let sql = format!(
        "SELECT {k}, trigger_kind, duration_ms, human_gap_ms, agent_type, COALESCE(model_raw, model) FROM agent_turns WHERE start_ts >= ?1 AND start_ts < ?2"
    );
    let mut st = c.prepare(&sql).map_err(err)?;
    let ttl_cfg = crate::meta::Meta::load().cache_ttl;
    // (turns, work ms, response ms, gaps ms, gaps past the harness/model cache TTL)
    let mut lat: HashMap<String, (i64, i64, Vec<i64>, Vec<i64>, usize)> = HashMap::new();
    for r in st
        .query_map([from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(err)?
    {
        let (key, trig, dur, gap, at, model) = r.map_err(err)?;
        let e = lat.entry(key).or_default();
        e.1 += dur.unwrap_or(0);
        if trig.as_deref() == Some("human") {
            e.0 += 1;
            if let Some(d) = dur {
                e.2.push(d);
            }
            if let Some(gp) = gap {
                e.3.push(gp);
                if gp > ttl_cfg.ttl(at.as_deref().unwrap_or(""), model.as_deref()).0 as i64 * 1000 {
                    e.4 += 1;
                }
            }
        }
    }
    for (key, (turns, work, mut resp, mut gaps, after_ttl)) in lat {
        let n_gaps = gaps.len();
        let after60 = gaps.iter().filter(|g| **g > 3_600_000).count();
        let e = entry(&mut groups, &key);
        e["turns"] = json!(turns);
        e["workingHours"] = json!(round(work as f64 / 3_600_000.0, 2));
        e["medianAgentResponseSec"] = json!(median(&mut resp).map(|m| round(m / 1000.0, 1)));
        e["medianHumanResponseSec"] = json!(median(&mut gaps).map(|m| round(m / 1000.0, 1)));
        if n_gaps > 0 {
            e["humanResponsesAfterCacheTtl"] = json!(round(after_ttl as f64 / n_gaps as f64, 3));
            e["humanResponsesAfterCache60m"] = json!(round(after60 as f64 / n_gaps as f64, 3));
        }
    }

    // tools: top 5 per group
    let k = key_expr(by, "ts", false)?;
    let k = if by == "agent" { "COALESCE((SELECT agent_name FROM agent_turns t WHERE t.rec_key = tool_calls.turn_key), '-')".to_string() } else { k };
    let sql = format!("SELECT {k}, tool, COUNT(*) FROM tool_calls WHERE ts >= ?1 AND ts < ?2 GROUP BY 1, 2 ORDER BY 1, 3 DESC");
    let mut st = c.prepare(&sql).map_err(err)?;
    for r in st.query_map([from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))).map_err(err)? {
        let (key, tool, n) = r.map_err(err)?;
        let e = entry(&mut groups, &key);
        let arr = e["topTools"].as_array_mut().unwrap();
        if arr.len() < 5 {
            arr.push(json!([tool, n]));
        }
    }

    // tokens and cost
    let k = key_expr(by, "ts", false)?;
    let sql = format!(
        "SELECT {k}, SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens), SUM(cache_write_tokens), SUM(cost) FROM usage_events WHERE ts >= ?1 AND ts < ?2 GROUP BY 1"
    );
    let mut st = c.prepare(&sql).map_err(err)?;
    for r in st
        .query_map([from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
            ))
        })
        .map_err(err)?
    {
        let (key, i, o, cr, cw, cost) = r.map_err(err)?;
        if by == "agent" {
            continue; // usage rows carry no agent name
        }
        let e = entry(&mut groups, &key);
        e["tokens"] = json!({"input": i.unwrap_or(0), "output": o.unwrap_or(0), "cacheRead": cr.unwrap_or(0), "cacheWrite": cw.unwrap_or(0)});
        e["cost"] = json!(round(cost.unwrap_or(0.0), 2));
    }

    let mut list: Vec<Value> = groups.into_values().collect();
    if matches!(by, "day" | "hour" | "weekday") {
        list.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    } else {
        list.sort_by(|a, b| b["messages"].as_i64().cmp(&a["messages"].as_i64()).then(b["turns"].as_i64().cmp(&a["turns"].as_i64())));
    }
    Ok(json!({"from": from, "to": if to == i64::MAX { Value::Null } else { json!(to) }, "by": by, "groups": list}))
}

/// Plain-text table for the CLI.
pub fn render(v: &Value, top: usize) -> String {
    let mut out = format!(
        "{:<26} {:>7} {:>5} {:>6} {:>8} {:>9} {:>9} {:>6} {:>6} {:>9}  top tools\n",
        v["by"].as_str().unwrap_or(""),
        "msgs",
        "days",
        "turns",
        "work h",
        "agent med",
        "human med",
        ">ttl",
        ">60m",
        "cost $"
    );
    let f = |x: &Value, d: usize| x.as_f64().map(|n| format!("{n:.d$}")).unwrap_or_else(|| "-".into());
    for gr in v["groups"].as_array().into_iter().flatten().take(top) {
        let tools: Vec<String> = gr["topTools"].as_array().into_iter().flatten().take(3).map(|t| format!("{} {}", t[0].as_str().unwrap_or(""), t[1])).collect();
        let key: String = gr["key"].as_str().unwrap_or("").chars().take(26).collect();
        out.push_str(&format!(
            "{:<26} {:>7} {:>5} {:>6} {:>8} {:>8}s {:>8}s {:>6} {:>6} {:>9}  {}\n",
            key,
            gr["messages"],
            gr["activeDays"],
            gr["turns"],
            f(&gr["workingHours"], 1),
            f(&gr["medianAgentResponseSec"], 0),
            f(&gr["medianHumanResponseSec"], 0),
            f(&gr["humanResponsesAfterCacheTtl"], 2),
            f(&gr["humanResponsesAfterCache60m"], 2),
            f(&gr["cost"], 2),
            tools.join(", ")
        ));
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Highlighted snippet (\u{1}/\u{2} sentinels → <mark>) with everything else HTML-escaped.
fn mark(s: &str) -> String {
    html_escape(s).replace('\u{1}', "<mark>").replace('\u{2}', "</mark>")
}

pub struct SearchQuery<'a> {
    pub q: &'a str,
    pub project: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub from: i64,
    pub to: i64,
    pub limit: usize,
}

/// Full-text search over human and assistant messages (`messages_fts`, trigram). Terms shorter
/// than 3 characters (common in Korean) fall back to a substring scan.
pub fn search(db: &Db, sq: &SearchQuery) -> Res<Vec<Value>> {
    let terms: Vec<&str> = sq.q.split_whitespace().filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let limit = sq.limit.clamp(1, 200) as i64;
    let fts = terms.iter().all(|t| t.chars().count() >= 3);
    let filters =
        "AND (?3 IS NULL OR s.project = ?3) AND (?4 IS NULL OR s.names LIKE '%' || ?4 || '%' OR s.titles LIKE '%' || ?4 || '%') AND f.ts >= ?5 AND f.ts < ?6";
    let sql = if fts {
        format!(
            "SELECT f.rec_key, f.session_id, f.ts, f.role, snippet(messages_fts, 0, char(1), char(2), '…', 16), s.file, s.agent_type, s.project, s.names, s.titles
             FROM messages_fts f JOIN sessions s ON s.id = f.session_id WHERE messages_fts MATCH ?1 {filters} ORDER BY bm25(messages_fts) LIMIT ?2"
        )
    } else {
        format!(
            "SELECT f.rec_key, f.session_id, f.ts, f.role, substr(f.text, max(1, instr(f.text, ?7) - 40), 120), s.file, s.agent_type, s.project, s.names, s.titles
             FROM messages_fts f JOIN sessions s ON s.id = f.session_id WHERE ?1 IS NOT NULL AND {like} {filters} ORDER BY f.ts DESC LIMIT ?2",
            like = terms.iter().map(|t| format!("f.text LIKE '%{}%'", t.replace('\'', "''").replace(['%', '_'], ""))).collect::<Vec<_>>().join(" AND ")
        )
    };
    let match_expr = terms.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" AND ");
    let mut st = db.conn.prepare(&sql).map_err(|e| e.to_string())?;
    let first = terms[0].to_string();
    let params: Vec<&dyn rusqlite::ToSql> = if fts {
        vec![&match_expr, &limit, &sq.project, &sq.agent, &sq.from, &sq.to]
    } else {
        // ?1 is unused by the LIKE form; keep the numbering
        vec![&match_expr, &limit, &sq.project, &sq.agent, &sq.from, &sq.to, &first]
    };
    let rows = st
        .query_map(params.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        let (key, sid, ts, role, snip, file, agent_type, project, names, titles) = r.map_err(|e| e.to_string())?;
        let raw_id = key.rsplit_once('#').map(|x| x.1.to_string()).unwrap_or_default();
        // transcript message ids: omp record id; agy step "agy<idx>"
        let message_id = if agent_type == "antigravity" && raw_id.chars().all(|c| c.is_ascii_digit()) { format!("agy{raw_id}") } else { raw_id };
        let snippet = if fts {
            mark(snip.as_deref().unwrap_or(""))
        } else {
            // highlight the substring hits ourselves
            let mut s = html_escape(snip.as_deref().unwrap_or(""));
            for t in &terms {
                s = s.replace(&html_escape(t), &format!("<mark>{}</mark>", html_escape(t)));
            }
            s
        };
        let names: Vec<String> = serde_json::from_str(&names).unwrap_or_default();
        let titles: Vec<String> = serde_json::from_str(&titles).unwrap_or_default();
        out.push(json!({
            "session": sid, "file": file, "agentType": agent_type, "project": project,
            "agent": names.last().cloned().or_else(|| titles.last().cloned()),
            "role": role, "ts": ts, "snippet": snippet, "messageId": message_id, "live": false,
        }));
    }
    Ok(out)
}

/// Sessions (main sessions, not subagents/test runs) whose titles, adash names, project or
/// first prompt (its first 2000 characters) contain every term (case-insensitive; spaces ignored,
/// so "관측계획" finds "관측 계획"), newest first: one row per session, for finding a
/// past agent by what it was about and resuming it. An empty query lists the most recent sessions.
pub fn search_sessions(db: &Db, q: &str, limit: usize) -> Res<Vec<Value>> {
    let terms: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let cond = if terms.is_empty() {
        "1".to_string()
    } else {
        (0..terms.len()).map(|i| format!("instr(replace(lower(hay), ' ', ''), ?{}) > 0", i + 2)).collect::<Vec<_>>().join(" AND ")
    };
    let sql = format!(
        // first prompts from human_messages (a plain table: the FTS table is ~50× slower to scan)
        // cut to 2000 chars, since pasted files make whole prompts huge
        "WITH fp AS MATERIALIZED (SELECT session_id, substr(text, 1, 2000) AS text, MIN(ts) AS ts FROM human_messages GROUP BY session_id),
              c AS (SELECT s.id, s.file, s.agent_type, s.project, s.cwd, s.names, s.titles, s.first_ts, s.last_ts, fp.text AS prompt,
                           s.titles || ' ' || s.names || ' ' || COALESCE(s.project, '') || ' ' || COALESCE(fp.text, '') AS hay
                    FROM sessions s LEFT JOIN fp ON fp.session_id = s.id
                    WHERE s.is_subagent = 0 AND COALESCE(s.is_test, 0) = 0)
         SELECT id, file, agent_type, project, cwd, names, titles, first_ts, last_ts, prompt FROM c WHERE {cond} ORDER BY last_ts DESC LIMIT ?1"
    );
    let limit = limit.clamp(1, 200) as i64;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&limit];
    params.extend(terms.iter().map(|t| t as &dyn rusqlite::ToSql));
    let mut st = db.conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        let (sid, file, agent_type, project, cwd, names, titles, first_ts, last_ts, prompt) = r.map_err(|e| e.to_string())?;
        let names: Vec<String> = serde_json::from_str(&names).unwrap_or_default();
        let titles: Vec<String> = serde_json::from_str(&titles).unwrap_or_default();
        let prompt = prompt.map(|p| crate::util::first_line(&p, 160)).filter(|p| !p.is_empty());
        let title = titles.last().cloned().or_else(|| prompt.clone()).unwrap_or_default();
        out.push(json!({
            "session": sid, "file": file, "agentType": agent_type, "project": project, "cwd": cwd,
            "agent": names.last().cloned(), "names": names, "titles": titles, "title": title, "prompt": prompt,
            "firstTs": first_ts, "lastTs": last_ts, "live": false,
        }));
    }
    Ok(out)
}

/// Agent→agent traffic in [from, to): per sender→receiver pair the messages (agent tells and
/// review-script pointers with a known sender, adash relay lines), review/propose requests
/// (threads) with their answer latency, and magi runs.
pub fn pairs(db: &Db, from: i64, to: i64) -> Res<Value> {
    let mut map: BTreeMap<(String, String), Value> = BTreeMap::new();
    let blank = || json!({"messages": 0, "agent": 0, "peer": 0, "relay": 0, "threads": 0, "review": 0, "propose": 0, "answered": 0, "answerMin": []});
    let mut st = db
        .conn
        .prepare(
            "SELECT m.from_agent, COALESCE(m.agent_name, (SELECT json_extract(s.names, '$[#-1]') FROM sessions s WHERE s.id = m.session_id), '-'), m.kind, COUNT(*)
             FROM human_messages m WHERE m.kind IN ('agent', 'peer', 'relay') AND m.from_agent IS NOT NULL AND m.ts >= ?1 AND m.ts < ?2 GROUP BY 1, 2, 3",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String, i64)> = st
        .query_map(rusqlite::params![from, to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    for (f, t, kind, n) in rows {
        let e = map.entry((f, t)).or_insert_with(blank);
        e[kind.as_str()] = json!(e[kind.as_str()].as_i64().unwrap_or(0) + n);
        e["messages"] = json!(e["messages"].as_i64().unwrap_or(0) + n);
    }
    let threads: Vec<crate::threads::Thread> =
        crate::threads::load_all().into_iter().filter(|t| t.from != "human" && t.created_at >= from && t.created_at < to).collect();
    for t in &threads {
        let e = map.entry((t.from.clone(), t.to.clone())).or_insert_with(blank);
        e["threads"] = json!(e["threads"].as_i64().unwrap_or(0) + 1);
        let k = if t.magi_kind.as_deref() == Some("propose") { "propose" } else { "review" };
        e[k] = json!(e[k].as_i64().unwrap_or(0) + 1);
        if let Some(at) = t.events.iter().find(|ev| ev.kind == "answered").map(|ev| ev.at) {
            e["answered"] = json!(e["answered"].as_i64().unwrap_or(0) + 1);
            e["answerMin"].as_array_mut().unwrap().push(json!(((at - t.created_at) as f64 / 60_000.0 * 10.0).round() / 10.0));
        }
    }
    let mut out: Vec<Value> = map
        .into_iter()
        .map(|((f, t), mut v)| {
            let mut mins: Vec<f64> = v["answerMin"].as_array().unwrap().iter().filter_map(Value::as_f64).collect();
            mins.sort_by(f64::total_cmp);
            v["medianAnswerMin"] = if mins.is_empty() { Value::Null } else { json!(mins[mins.len() / 2]) };
            v.as_object_mut().unwrap().remove("answerMin");
            v["from"] = json!(f);
            v["to"] = json!(t);
            v
        })
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v["messages"].as_i64().unwrap_or(0) + v["threads"].as_i64().unwrap_or(0)));
    let sum = |k: &str| out.iter().map(|v| v[k].as_i64().unwrap_or(0)).sum::<i64>();
    let (rev, pro) = (sum("review"), sum("propose"));
    let magi = crate::magi::RunState::load_all().iter().filter(|r| r.created_at >= from && r.created_at < to).count();
    let mut all: Vec<f64> =
        threads.iter().filter_map(|t| t.events.iter().find(|ev| ev.kind == "answered").map(|ev| (ev.at - t.created_at) as f64 / 60_000.0)).collect();
    all.sort_by(f64::total_cmp);
    Ok(json!({
        "from": from, "to": to, "by": "pair", "pairs": out,
        "totals": {
            "messages": sum("messages"), "threads": sum("threads"), "review": rev, "propose": pro,
            "reviewShare": if rev + pro > 0 { json!((rev as f64 / (rev + pro) as f64 * 100.0).round() / 100.0) } else { Value::Null },
            "medianAnswerMin": all.get(all.len() / 2).map(|m| (m * 10.0).round() / 10.0),
            "magiRuns": magi,
        },
    }))
}

pub fn render_pairs(v: &Value, top: usize) -> String {
    let t = &v["totals"];
    let mut s = format!(
        "에이전트 간: 메시지 {} · 검토 {} · 제안 {} · 답변 중앙값 {}분 · magi {}회\n\n{:<24} {:<24} {:>6} {:>6} {:>6} {:>6} {:>8}\n",
        t["messages"],
        t["review"],
        t["propose"],
        t["medianAnswerMin"].as_f64().map(|m| format!("{m:.1}")).unwrap_or("-".into()),
        t["magiRuns"],
        "보낸 쪽",
        "받은 쪽",
        "메시지",
        "검토",
        "제안",
        "답함",
        "중앙값(분)"
    );
    for p in v["pairs"].as_array().into_iter().flatten().take(top) {
        let _ = std::fmt::Write::write_fmt(
            &mut s,
            format_args!(
                "{:<24} {:<24} {:>6} {:>6} {:>6} {:>6} {:>8}\n",
                crate::util::truncate(p["from"].as_str().unwrap_or(""), 22),
                crate::util::truncate(p["to"].as_str().unwrap_or(""), 22),
                p["messages"],
                p["review"],
                p["propose"],
                p["answered"],
                p["medianAnswerMin"].as_f64().map(|m| format!("{m:.1}")).unwrap_or("-".into())
            ),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_korean_substrings_and_escapes_html() {
        let dir = std::env::temp_dir().join(format!("adash-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::analytics::open_at(&dir.join("a.db")).unwrap();
        db.conn.execute("INSERT INTO sessions(file, agent_type, project, names) VALUES ('/s/a.jsonl', 'omp', 'galaxy-survey', '[\"PLANNER\"]')", []).unwrap();
        for (k, ts, role, text) in [
            ("/s/a.jsonl#u1", 1000, "user", "제안서 초안을 <b>검토</b>해 주세요"),
            ("/s/a.jsonl#a1", 2000, "assistant", "초안 검토를 마쳤습니다. bandwidth law 문단을 줄였습니다."),
            ("/s/a.jsonl#u2", 3000, "user", "다른 이야기"),
        ] {
            db.conn
                .execute("INSERT INTO messages_fts(text, rec_key, session_id, ts, role) VALUES (?1, ?2, 1, ?3, ?4)", rusqlite::params![text, k, ts, role])
                .unwrap();
        }
        let q = |s: &str| search(&db, &SearchQuery { q: s, project: None, agent: None, from: 0, to: i64::MAX, limit: 10 }).unwrap();
        // ≥3-character terms: FTS trigram
        let hits = q("bandwidth law");
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0]["messageId"].as_str(), hits[0]["role"].as_str(), hits[0]["agent"].as_str()), (Some("a1"), Some("assistant"), Some("PLANNER")));
        assert!(hits[0]["snippet"].as_str().unwrap().contains("<mark>bandwidth</mark>"));
        // 2-character Korean term: substring scan, newest first, HTML escaped except <mark>
        let hits = q("초안");
        assert_eq!(hits.iter().map(|h| h["messageId"].as_str().unwrap()).collect::<Vec<_>>(), ["a1", "u1"]);
        let snip = hits[1]["snippet"].as_str().unwrap();
        assert!(snip.contains("<mark>초안</mark>") && snip.contains("&lt;b&gt;") && !snip.contains("<b>"));
        assert!(q("없는말").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_search_matches_titles_and_first_prompt_but_not_subagents() {
        let dir = std::env::temp_dir().join(format!("adash-ssearch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::analytics::open_at(&dir.join("a.db")).unwrap();
        for (file, titles, sub, last) in
            [("/s/a.jsonl", "[\"ARC: 망원경 적합 검토\"]", 0, 10), ("/s/b.jsonl", "[]", 0, 20), ("/s/a/sub.jsonl", "[\"ARC subagent\"]", 1, 30)]
        {
            db.conn
                .execute(
                    "INSERT INTO sessions(file, agent_type, project, titles, is_subagent, last_ts) VALUES (?1, 'omp', 'galaxy-survey', ?2, ?3, ?4)",
                    rusqlite::params![file, titles, sub, last],
                )
                .unwrap();
        }
        // session 2 has no title: its first prompt (not the later one) is what it is about
        for (k, sid, ts, text) in [("/s/b.jsonl#u1", 2, 1, "Inbox 메일 정리해줘"), ("/s/b.jsonl#u2", 2, 2, "ARC 일정도 봐줘")] {
            db.conn
                .execute(
                    "INSERT INTO human_messages(rec_key, session_id, ts, kind, channel, text) VALUES (?2, ?3, ?4, 'human', 'terminal', ?1)",
                    rusqlite::params![text, k, sid, ts],
                )
                .unwrap();
        }
        let files = |q: &str| search_sessions(&db, q, 10).unwrap().iter().map(|h| h["file"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(files("arc"), ["/s/a.jsonl"]);
        assert_eq!(files("inbox 메일"), ["/s/b.jsonl"]);
        assert_eq!(search_sessions(&db, "inbox", 10).unwrap()[0]["title"], "Inbox 메일 정리해줘");
        assert_eq!(files("galaxy"), ["/s/b.jsonl", "/s/a.jsonl"]);
        // spaces are ignored on both sides; an empty query lists recent sessions
        assert_eq!(files("적합검토"), ["/s/a.jsonl"]);
        assert_eq!(files(""), ["/s/b.jsonl", "/s/a.jsonl"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
