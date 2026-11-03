//! Phase D: weekly / monthly activity reports from analytics.db — self-contained HTML (inline
//! SVG, Korean) + PDF through headless Chrome. Every number comes from a SQL query listed in
//! the appendix with its bound parameters (`?1` = from ms, `?2` = to ms).
use crate::analytics::Db;
use crate::util::{atomic_write, now_ms, Res};
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

pub fn dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-reports-{}", std::process::id()));
    }
    crate::platform::data_dir().join("reports")
}

fn local_ms(d: NaiveDate) -> i64 {
    Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().map(|t| t.timestamp_millis()).unwrap_or(0)
}

/// (period, label, from, to). week: last full ISO week; month: `prev` → last full month,
/// else the current month so far; custom: explicit dates (to exclusive, next midnight).
pub fn bounds(period: &str, from: Option<&str>, to: Option<&str>, prev: bool) -> Res<(String, String, i64, i64)> {
    let today = Local::now().date_naive();
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("bad date {s} (YYYY-MM-DD)"));
    match (period, from) {
        (_, Some(f)) => {
            let f = parse(f)?;
            let t = to.map(parse).transpose()?.unwrap_or(today);
            if t < f {
                return Err("to is before from".into());
            }
            // the period names a folder: only these three
            if !matches!(period, "week" | "month" | "custom") {
                return Err(format!("period must be week|month|custom, not {period}"));
            }
            let p = period;
            let label = match p {
                "week" => format!("{}-W{:02}", f.iso_week().year(), f.iso_week().week()),
                "month" => format!("{}-{:02}", f.year(), f.month()),
                _ => format!("{}_{}", f.format("%Y%m%d"), t.format("%Y%m%d")),
            };
            Ok((p.into(), label, local_ms(f), local_ms(t.succ_opt().unwrap())))
        }
        ("week", None) => {
            let this_monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
            let start = this_monday - chrono::Duration::days(7);
            Ok(("week".into(), format!("{}-W{:02}", start.iso_week().year(), start.iso_week().week()), local_ms(start), local_ms(this_monday)))
        }
        ("month", None) => {
            let first = today.with_day(1).unwrap();
            if prev {
                let pf = (first - chrono::Duration::days(1)).with_day(1).unwrap();
                Ok(("month".into(), format!("{}-{:02}", pf.year(), pf.month()), local_ms(pf), local_ms(first)))
            } else {
                Ok(("month".into(), format!("{}-{:02}", first.year(), first.month()), local_ms(first), local_ms(today.succ_opt().unwrap()).min(now_ms())))
            }
        }
        _ => Err("period must be week|month|custom (custom needs --from)".into()),
    }
}

pub(crate) struct Ctx<'a> {
    pub(crate) db: &'a Db,
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) appendix: Vec<(String, String)>,
}

impl Ctx<'_> {
    /// Record a query for the appendix; returns its index (anchor `sql-<i>`).
    pub(crate) fn note(&mut self, label: &str, sql: &str) -> usize {
        self.appendix.push((label.into(), sql.split_whitespace().collect::<Vec<_>>().join(" ")));
        self.appendix.len() - 1
    }
    /// "근거" link to the last recorded query (or `i`).
    pub(crate) fn src(&self, i: Option<usize>) -> String {
        let i = i.unwrap_or(self.appendix.len().saturating_sub(1));
        format!(" <a class=src href=\"#sql-{i}\">근거</a>")
    }
    /// Rows as JSON values (text or numbers), for queries returning labels.
    pub(crate) fn table(&mut self, label: &str, sql: &str) -> Vec<Vec<Value>> {
        self.note(label, sql);
        let Ok(mut st) = self.db.conn.prepare(sql) else { return vec![] };
        let n = st.column_count();
        st.query_map(params![self.from, self.to], |r| {
            Ok((0..n)
                .map(|i| match r.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Integer(x)) => json!(x),
                    Ok(rusqlite::types::ValueRef::Real(x)) => json!(x),
                    Ok(rusqlite::types::ValueRef::Text(t)) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                })
                .collect())
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
    pub(crate) fn table_at(&self, sql: &str, from: i64, to: i64) -> Vec<Vec<Value>> {
        let Ok(mut st) = self.db.conn.prepare(sql) else { return vec![] };
        let n = st.column_count();
        st.query_map(params![from, to], |r| {
            Ok((0..n)
                .map(|i| match r.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Integer(x)) => json!(x),
                    Ok(rusqlite::types::ValueRef::Real(x)) => json!(x),
                    Ok(rusqlite::types::ValueRef::Text(t)) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                })
                .collect())
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
    fn scalar(&mut self, label: &str, sql: &str) -> f64 {
        self.note(label, sql);
        self.db.conn.query_row(sql, params![self.from, self.to], |r| r.get::<_, Option<f64>>(0)).ok().flatten().unwrap_or(0.0)
    }
    fn scalar_at(&self, sql: &str, from: i64, to: i64) -> f64 {
        self.db.conn.query_row(sql, params![from, to], |r| r.get::<_, Option<f64>>(0)).ok().flatten().unwrap_or(0.0)
    }
    /// rows of (key, value…)
    fn rows(&mut self, label: &str, sql: &str) -> Vec<(String, Vec<f64>)> {
        self.note(label, sql);
        let Ok(mut st) = self.db.conn.prepare(sql) else { return vec![] };
        let n = st.column_count();
        st.query_map(params![self.from, self.to], |r| {
            let k: Option<String> = r.get(0).ok();
            Ok((k.unwrap_or_else(|| "(없음)".into()), (1..n).map(|i| r.get::<_, Option<f64>>(i).ok().flatten().unwrap_or(0.0)).collect()))
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
}

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub(crate) fn fmt_n(x: f64) -> String {
    if x.abs() >= 100.0 || x.fract() == 0.0 {
        let s = format!("{:.0}", x);
        let neg = s.starts_with('-');
        let d: Vec<char> = s.trim_start_matches('-').chars().collect();
        let mut out = String::new();
        for (i, c) in d.iter().enumerate() {
            if i > 0 && (d.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(*c);
        }
        if neg {
            format!("-{out}")
        } else {
            out
        }
    } else {
        format!("{x:.1}")
    }
}

pub(crate) fn delta(cur: f64, prev: f64) -> String {
    if prev <= 0.0 {
        return if cur > 0.0 { "<span class=up>신규</span>".into() } else { String::new() };
    }
    let p = (cur - prev) / prev * 100.0;
    let cls = if p >= 0.0 { "up" } else { "down" };
    format!("<span class={cls}>{}{:.0}%</span>", if p >= 0.0 { "+" } else { "" }, p)
}

/// Horizontal bars (top `n`): key, value, display text, Δ.
fn bars(items: &[(String, f64, String, String)], n: usize) -> String {
    let max = items.iter().map(|x| x.1).fold(0.0, f64::max).max(1e-9);
    let rows = items.iter().take(n).count();
    let mut s = format!("<svg viewBox='0 0 640 {}' class=chart>", rows * 26 + 4);
    for (i, (k, v, txt, d)) in items.iter().take(n).enumerate() {
        let y = i * 26 + 2;
        let w = (v / max * 240.0).max(1.0);
        let _ = write!(
            s,
            "<text x=0 y={} class=lbl>{}</text><rect x=170 y={} width={w:.1} height=18 rx=3 class=bar /><text x={:.1} y={} class=val>{} {}</text>",
            y + 14,
            esc(&crate::util::truncate(k, 24)),
            y,
            176.0 + w,
            y + 14,
            esc(txt),
            d.replace("<span class=up>", "").replace("<span class=down>", "").replace("</span>", "")
        );
    }
    s.push_str("</svg>");
    s
}

fn heatmap(cells: &[[f64; 24]; 7]) -> String {
    let max = cells.iter().flatten().cloned().fold(0.0, f64::max).max(1.0);
    let days = ["월", "화", "수", "목", "금", "토", "일"];
    let mut s = String::from("<svg viewBox='0 0 640 200' class=chart>");
    for h in 0..24 {
        if h % 3 == 0 {
            let _ = write!(s, "<text x={} y=12 class=ax>{h}</text>", 30 + h * 25);
        }
    }
    for (d, row) in cells.iter().enumerate() {
        let _ = write!(s, "<text x=0 y={} class=ax>{}</text>", 32 + d * 24, days[d]);
        for (h, v) in row.iter().enumerate() {
            let o = if *v > 0.0 { 0.12 + 0.88 * v / max } else { 0.04 };
            let _ = write!(
                s,
                "<rect x={} y={} width=23 height=22 rx=3 class=heat fill-opacity={o:.2}><title>{} {h}시: {v:.0}</title></rect>",
                28 + h * 25,
                18 + d * 24,
                days[d]
            );
        }
    }
    s.push_str("</svg>");
    s
}

fn columns(vals: &[(String, f64)], h: f64) -> String {
    let max = vals.iter().map(|x| x.1).fold(0.0, f64::max).max(1e-9);
    let w = 600.0 / vals.len().max(1) as f64;
    let mut s = format!("<svg viewBox='0 0 640 {}' class=chart>", h as i64 + 34);
    for (i, (k, v)) in vals.iter().enumerate() {
        let bh = v / max * h;
        let x = 20.0 + i as f64 * w;
        let _ = write!(
            s,
            "<rect x={x:.1} y={:.1} width={:.1} height={bh:.1} rx=2 class=bar><title>{}: {}</title></rect>",
            h + 4.0 - bh,
            (w - 3.0).max(1.0),
            esc(k),
            fmt_n(*v)
        );
        if vals.len() <= 12 || i % (vals.len() / 12 + 1) == 0 {
            let _ = write!(s, "<text x={:.1} y={} class=ax>{}</text>", x, h as i64 + 22, esc(k));
        }
    }
    s.push_str("</svg>");
    s
}

pub struct Built {
    pub id: String,
    pub html: PathBuf,
    pub pdf: Option<PathBuf>,
    pub meta: Value,
}

/// Build the report into `reports/<period>/<label>.{html,json[,pdf]}`.
pub fn build(db: &Db, period: &str, label: &str, from: i64, to: i64, pdf: bool) -> Res<Built> {
    let mut c = Ctx { db, from, to, appendix: vec![] };
    let span = to - from;
    let (pf, pt) = (from - span, from);
    const HM: &str = "FROM human_messages WHERE kind = 'human' AND ts >= ?1 AND ts < ?2";
    // time a turn's wait for me spent snoozed (snoozed segments of the session, clipped to the
    // gap before the human message): not waiting on me, so not a bottleneck
    const TURNS: &str = "FROM agent_turns WHERE start_ts >= ?1 AND start_ts < ?2";
    const SNOOZED_IN_GAP: &str = SNOOZED_OVERLAP;

    // ---- headline
    let msgs_sql = format!("SELECT COUNT(*) {HM}");
    let messages = c.scalar("보낸 메시지", &msgs_sql);
    let hours_sql = format!("SELECT COUNT(DISTINCT ts / 3600000) {HM}");
    let active_hours = c.scalar("활동 시간(메시지를 보낸 시간대 수)", &hours_sql);
    let agents_sql = "SELECT COUNT(DISTINCT t.session_id) FROM agent_turns t JOIN sessions s ON s.id = t.session_id WHERE s.is_subagent = 0 AND t.start_ts >= ?1 AND t.start_ts < ?2".to_string();
    let agents = c.scalar("사용한 에이전트(서브에이전트 제외 세션)", &agents_sql);
    let subagents = c.scalar(
        "서브에이전트 세션",
        "SELECT COUNT(DISTINCT t.session_id) FROM agent_turns t JOIN sessions s ON s.id = t.session_id WHERE s.is_subagent = 1 AND t.start_ts >= ?1 AND t.start_ts < ?2",
    );
    let turns_sql = format!("SELECT COUNT(*) {TURNS}");
    let turns = c.scalar("에이전트 턴", &turns_sql);
    let work_sql = format!("SELECT SUM(duration_ms) / 3600000.0 {TURNS}");
    let work_h = c.scalar("에이전트 작업 시간(h)", &work_sql);
    let cost_sql = "SELECT SUM(cost) FROM usage_events WHERE ts >= ?1 AND ts < ?2";
    let cost = c.scalar("비용($)", cost_sql);
    let tok_sql = "SELECT SUM(COALESCE(input_tokens,0) + COALESCE(output_tokens,0) + COALESCE(cache_read_tokens,0) + COALESCE(cache_write_tokens,0)) FROM usage_events WHERE ts >= ?1 AND ts < ?2";
    let tokens = c.scalar("토큰(입력+출력+캐시)", tok_sql);
    let prev = [
        c.scalar_at(&msgs_sql, pf, pt),
        c.scalar_at(&hours_sql, pf, pt),
        c.scalar_at(&agents_sql, pf, pt),
        c.scalar_at(&turns_sql, pf, pt),
        c.scalar_at(&work_sql, pf, pt),
        c.scalar_at(cost_sql, pf, pt),
    ];
    let cap = c.rows(
        "용량 최대 사용률(공급자·한도별)",
        "SELECT provider || ' · ' || COALESCE(window_label, label, limit_id), MAX(used_fraction) * 100 FROM capacity_snapshots WHERE ts >= ?1 AND ts < ?2 GROUP BY 1 ORDER BY 2 DESC LIMIT 6",
    );

    // ---- breakdowns with share and Δ vs previous period
    let mut sections = String::new();
    for (title, col) in [
        ("에이전트 종류", "agent_type"),
        ("모델 계열", "COALESCE(model_family(NULLIF(model, '')), '미상')"),
        ("모델", "COALESCE(NULLIF(model, ''), '미상')"),
        ("프로젝트", "project"),
        ("이름 붙은 에이전트", "agent_name"),
    ] {
        let sql = format!("SELECT {col}, COUNT(*), SUM(duration_ms) / 3600000.0, SUM(cost) {TURNS} GROUP BY 1 ORDER BY 2 DESC");
        let cur = c.rows(&format!("{title}별 턴 · 작업 시간(h) · 비용"), &sql);
        let prevm: BTreeMap<String, f64> = {
            let mut st = c.db.conn.prepare(&sql).map_err(|e| e.to_string())?;
            let m: BTreeMap<String, f64> = st
                .query_map(params![pf, pt], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "(없음)".into()), r.get::<_, f64>(1)?)))
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .collect();
            m
        };
        let total: f64 = cur.iter().map(|x| x.1[0]).sum::<f64>().max(1.0);
        let items: Vec<(String, f64, String, String)> = cur
            .iter()
            .filter(|(k, _)| col != "agent_name" || k != "(없음)")
            .map(|(k, v)| {
                (k.clone(), v[0], format!("{}턴 {:.0}% · {:.1}h", fmt_n(v[0]), v[0] / total * 100.0, v[1]), delta(v[0], *prevm.get(k).unwrap_or(&0.0)))
            })
            .collect();
        let _ = write!(sections, "<section><h2>{title}별</h2>{}</section>", bars(&items, 10));
    }

    // ---- provider/API errors (provider · status), recovered = omp auto-retry succeeded
    {
        let sql = "SELECT COALESCE(provider, '미상') || ' · ' || COALESCE(CAST(status AS TEXT), '상태 없음'), COUNT(*), SUM(recovered) FROM errors WHERE ts >= ?1 AND ts < ?2 GROUP BY 1 ORDER BY 2 DESC";
        let cur = c.rows("오류 기록(제공자 · 상태별 건수, 자동 재시도로 복구된 건수)", sql);
        let prevm: BTreeMap<String, f64> = {
            let mut st = c.db.conn.prepare(sql).map_err(|e| e.to_string())?;
            let m: BTreeMap<String, f64> = st
                .query_map(params![pf, pt], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .collect();
            m
        };
        let items: Vec<(String, f64, String, String)> = cur
            .iter()
            .map(|(k, v)| (k.clone(), v[0], format!("{}건 · 복구 {}", fmt_n(v[0]), fmt_n(v[1])), delta(v[0], *prevm.get(k).unwrap_or(&0.0))))
            .collect();
        let body = if items.is_empty() { "<p class=\"muted\">이 기간에 기록된 오류가 없습니다.</p>".to_string() } else { bars(&items, 10) };
        let _ = write!(sections, "<section><h2>오류 기록</h2>{body}</section>");
    }

    // ---- hour × weekday heatmap (local time) of my messages
    let hm_sql = format!("SELECT ts {HM}");
    c.note("시간대×요일 히트맵(로컬 시각으로 집계)", &hm_sql);
    let mut cells = [[0.0f64; 24]; 7];
    let mut day_set = std::collections::BTreeSet::new();
    let mut by_hour = [0.0f64; 24];
    {
        let mut st = c.db.conn.prepare(&hm_sql).map_err(|e| e.to_string())?;
        for ts in st.query_map(params![from, to], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?.filter_map(Result::ok) {
            if let Some(t) = Local.timestamp_millis_opt(ts).single() {
                cells[t.weekday().num_days_from_monday() as usize][t.hour() as usize] += 1.0;
                by_hour[t.hour() as usize] += 1.0;
                day_set.insert(t.date_naive());
            }
        }
    }

    // ---- my response time + cache expiry
    let rt_sql = format!(
        "SELECT human_gap_ms, agent_type, model_raw, model, cost * cache_write_tokens * 1.0 / NULLIF(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens, 0) {TURNS} AND trigger_kind = 'human' AND human_gap_ms IS NOT NULL"
    );
    c.note("내 응답 시간 분포(직전 턴 종료→다음 메시지), 턴마다 하네스·모델별 캐시 TTL과 비교", &rt_sql);
    let ttl_cfg = crate::meta::Meta::load().cache_ttl;
    // (gap, past the cache TTL of that harness/model, cache-write cost of the turn)
    let turns_gap: Vec<(i64, bool, f64)> = {
        let mut st = c.db.conn.prepare(&rt_sql).map_err(|e| e.to_string())?;
        let v = st
            .query_map(params![from, to], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|(g, at, raw, m, cw)| {
                let (ttl, _) = ttl_cfg.ttl(at.as_deref().unwrap_or(""), raw.as_deref().or(m.as_deref()));
                (g, g > ttl as i64 * 1000, cw.unwrap_or(0.0))
            })
            .collect();
        v
    };
    let gaps: Vec<i64> = turns_gap.iter().map(|t| t.0).collect();
    let buckets = [("<1분", 60_000i64), ("1–5분", 300_000), ("5–15분", 900_000), ("15–60분", 3_600_000), ("1–4시간", 14_400_000), ("4시간+", i64::MAX)];
    let mut lo = 0i64;
    let hist: Vec<(String, f64)> = buckets
        .iter()
        .map(|(k, hi)| {
            let n = gaps.iter().filter(|g| **g >= lo && **g < *hi).count() as f64;
            lo = *hi;
            (k.to_string(), n)
        })
        .collect();
    let gn = gaps.len().max(1) as f64;
    let after_ttl = turns_gap.iter().filter(|t| t.1).count() as f64 / gn * 100.0;
    let after60 = gaps.iter().filter(|g| **g > 3_600_000).count() as f64 / gn * 100.0;
    let lost: f64 = turns_gap.iter().filter(|t| t.1).map(|t| t.2).sum();

    // ---- parallelism from turn intervals
    let par_sql = format!("SELECT start_ts, COALESCE(end_ts, start_ts + COALESCE(duration_ms, 0)) {TURNS} AND duration_ms > 0");
    c.note("동시 작업 에이전트(턴 구간 겹침을 sweep 으로 계산)", &par_sql);
    let mut ev: Vec<(i64, i32)> = vec![];
    {
        let mut st = c.db.conn.prepare(&par_sql).map_err(|e| e.to_string())?;
        for (s, e) in st.query_map(params![from, to], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(|e| e.to_string())?.filter_map(Result::ok) {
            ev.push((s, 1));
            ev.push((e.min(to).max(s), -1));
        }
    }
    ev.sort();
    let (mut cur, mut maxc, mut busy_ms, mut weighted, mut last) = (0i32, 0i32, 0i64, 0i64, 0i64);
    for (t, d) in &ev {
        if cur > 0 {
            busy_ms += t - last;
            weighted += (t - last) * cur as i64;
        }
        cur += d;
        maxc = maxc.max(cur);
        last = *t;
    }
    let mean_par = if busy_ms > 0 { weighted as f64 / busy_ms as f64 } else { 0.0 };

    // ---- bottlenecks: hours agents waited on me, per project
    let wait = c.rows(
        "병목: 프로젝트별 나를 기다린 시간(h, 사람 응답 간격 합)",
        &format!("SELECT project, SUM(MAX(0, human_gap_ms - {SNOOZED_IN_GAP})) / 3600000.0, COUNT(*) {TURNS} AND trigger_kind = 'human' AND human_gap_ms IS NOT NULL GROUP BY 1 ORDER BY 2 DESC LIMIT 10"),
    );
    // ---- asks / threads / tools
    let asks = c.scalar("답한 질문(ask)", "SELECT COUNT(*) FROM asks WHERE answered_at >= ?1 AND answered_at < ?2 AND cancelled = 0");
    let asks_dash = c
        .scalar("대시보드에서 답한 질문", "SELECT COUNT(*) FROM asks WHERE answered_at >= ?1 AND answered_at < ?2 AND cancelled = 0 AND channel = 'dashboard'");
    let threads = c.scalar("에이전트 간 스레드(adash 중계)", "SELECT COUNT(*) FROM relay_threads WHERE created_at >= ?1 AND created_at < ?2");
    let peer = c.scalar("검토 스크립트 요청(peer_review·magi 포인터 줄)", "SELECT COUNT(*) FROM human_messages WHERE kind = 'peer' AND ts >= ?1 AND ts < ?2");
    let tools = c.rows("도구 상위 10", "SELECT tool, COUNT(*), SUM(is_error) FROM tool_calls WHERE ts >= ?1 AND ts < ?2 GROUP BY 1 ORDER BY 2 DESC LIMIT 10");

    // ---- html
    let fmt_range = |a: i64, b: i64| {
        let f = |x: i64| Local.timestamp_millis_opt(x).single().map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default();
        format!("{} ~ {}", f(a), f(b - 1))
    };
    let title = match period {
        "week" => format!("주간 보고서 {label}"),
        "month" => format!("월간 보고서 {label}"),
        _ => format!("보고서 {}", fmt_range(from, to)),
    };
    let card = |k: &str, v: String, d: String| format!("<div class=card><div class=k>{k}</div><div class=v>{v}</div><div class=d>{d}</div></div>");
    let mut h = String::new();
    let _ = write!(
        h,
        "<!doctype html><html lang=ko><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>{t}</title><style>{CSS}</style><body><header><h1>{t}</h1><p class=sub>{r} · 생성 {g} · 이전 기간 대비 변화는 같은 길이의 직전 기간과 비교</p></header>",
        t = esc(&title),
        r = fmt_range(from, to),
        g = Local::now().format("%Y-%m-%d %H:%M")
    );
    h.push_str("<section class=cards>");
    h.push_str(&card("보낸 메시지", fmt_n(messages), delta(messages, prev[0])));
    h.push_str(&card("활동 시간", format!("{}h", fmt_n(active_hours)), delta(active_hours, prev[1])));
    h.push_str(&card("에이전트", fmt_n(agents), format!("서브에이전트 {} {}", fmt_n(subagents), delta(agents, prev[2]))));
    h.push_str(&card("턴", fmt_n(turns), delta(turns, prev[3])));
    h.push_str(&card("에이전트 작업", format!("{work_h:.1}h"), delta(work_h, prev[4])));
    h.push_str(&card("비용 · 토큰", format!("${cost:.2}"), format!("{} 토큰 {}", fmt_n(tokens), delta(cost, prev[5]))));
    h.push_str("</section>");
    if !cap.is_empty() {
        let items: Vec<_> = cap.iter().map(|(k, v)| (k.clone(), v[0], format!("최대 {:.0}%", v[0]), String::new())).collect();
        let _ = write!(h, "<section><h2>용량 최대 사용률</h2>{}</section>", bars(&items, 6));
    }
    h.push_str(&sections);
    let _ = write!(h, "<section><h2>내 메시지: 시간대 × 요일</h2>{}</section>", heatmap(&cells));
    let _ = write!(
        h,
        "<section><h2>내 응답 시간</h2>{}<p>응답 {}건 중 캐시가 식은 뒤(하네스·모델별 TTL 초과: omp·Claude+Anthropic 60분, OpenAI 40분, 그 밖 5분) <b>{after_ttl:.0}%</b>, 1시간 초과 <b>{after60:.0}%</b>. 식은 뒤 턴의 캐시 쓰기 비용 추정 <b>${lost:.2}</b>.</p></section>",
        columns(&hist, 120.0),
        fmt_n(gaps.len() as f64)
    );
    let _ = write!(
        h,
        "<section><h2>병렬 작업</h2><p>동시에 일한 에이전트 최대 <b>{maxc}</b>, 하나라도 일한 시간 {:.1}h 동안 평균 <b>{mean_par:.2}</b>.</p></section>",
        busy_ms as f64 / 3.6e6
    );
    if !wait.is_empty() {
        let items: Vec<_> = wait.iter().map(|(k, v)| (k.clone(), v[0], format!("{:.1}h · {}회", v[0], fmt_n(v[1])), String::new())).collect();
        let _ = write!(h, "<section><h2>병목: 나를 기다린 시간</h2>{}</section>", bars(&items, 10));
    }
    let _ = write!(
        h,
        "<section><h2>질문 · 스레드</h2><p>답한 질문 <b>{}</b>(대시보드에서 {}) · adash 중계 스레드 <b>{}</b> · 검토 스크립트 요청 <b>{}</b></p></section>",
        fmt_n(asks),
        fmt_n(asks_dash),
        fmt_n(threads),
        fmt_n(peer)
    );
    // agent→agent traffic (sender → receiver)
    c.note(
        "에이전트 간 메시지(보낸 쪽→받은 쪽; 검토·제안은 스레드 파일, magi 는 실행 기록)",
        "SELECT from_agent, agent_name, kind, COUNT(*) FROM human_messages WHERE kind IN ('agent','peer','relay') AND from_agent IS NOT NULL AND ts >= ?1 AND ts < ?2 GROUP BY 1,2,3",
    );
    if let Ok(pv) = crate::stats::pairs(c.db, from, to) {
        let t = &pv["totals"];
        let items: Vec<(String, f64, String, String)> = pv["pairs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let n = p["messages"].as_f64().unwrap_or(0.0) + p["threads"].as_f64().unwrap_or(0.0);
                (
                    format!("{} → {}", p["from"].as_str().unwrap_or(""), p["to"].as_str().unwrap_or("")),
                    n,
                    format!("메시지 {} · 검토 {} · 제안 {}", p["messages"], p["review"], p["propose"]),
                    String::new(),
                )
            })
            .collect();
        let _ = write!(
            h,
            "<section><h2>에이전트 간</h2><p>메시지 <b>{}</b> · 검토 요청 <b>{}</b> · 제안 요청 <b>{}</b> · 답변 중앙값 <b>{}</b> · magi <b>{}</b>회</p>{}</section>",
            fmt_n(t["messages"].as_f64().unwrap_or(0.0)),
            fmt_n(t["review"].as_f64().unwrap_or(0.0)),
            fmt_n(t["propose"].as_f64().unwrap_or(0.0)),
            t["medianAnswerMin"].as_f64().map(|m| format!("{m:.0}분")).unwrap_or("-".into()),
            t["magiRuns"],
            if items.is_empty() { String::new() } else { bars(&items, 10) }
        );
    }
    if !tools.is_empty() {
        let items: Vec<_> = tools.iter().map(|(k, v)| (k.clone(), v[0], format!("{}회 · 오류 {}", fmt_n(v[0]), fmt_n(v[1])), String::new())).collect();
        let _ = write!(h, "<section><h2>도구 상위 10</h2>{}</section>", bars(&items, 10));
    }
    h.push_str(&crate::fun::awards(&mut c));
    h.push_str(&crate::fun::section(&mut c, (pf, pt)));
    // appendix: every query folded (CSS-only toggles: the report runs without scripts); a "근거"
    // link opens its query through :target; the PDF prints all of them small
    h.push_str("<section class=appendix><h2>부록: 계산에 쓴 SQL</h2><input type=checkbox id=sqlall class=tgall><label for=sqlall class=tgl>SQL 모두 펼치기/접기</label><p>?1 = ");
    let _ = write!(h, "{from}, ?2 = {to} (Unix ms). DB: ~/.local/share/adash/analytics.db</p><ol>");
    for (i, (k, sql)) in c.appendix.iter().enumerate() {
        let _ = write!(
            h,
            "<li id=sql-{i}><input type=checkbox id=t{i} class=tg><label for=t{i}><b>{}</b> <span class=sv>SQL 보기</span></label><pre>{}</pre></li>",
            esc(k),
            esc(sql)
        );
    }
    h.push_str("</ol></section></body></html>");

    let id = format!("{period}-{label}");
    let d = dir().join(period);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let html = d.join(format!("{label}.html"));
    atomic_write(&html, h.as_bytes())?;
    let pdf_path = if pdf { Some(print_pdf(&html)?) } else { None };
    let meta = json!({
        "id": id, "period": period, "title": title, "from": from, "to": to, "generatedAt": now_ms(), "hasPdf": pdf_path.is_some(),
        "headline": {"messages": messages, "activeHours": active_hours, "agents": agents, "turns": turns, "agentWorkHours": (work_h * 10.0).round() / 10.0, "cost": (cost * 100.0).round() / 100.0},
    });
    atomic_write(&d.join(format!("{label}.json")), serde_json::to_string_pretty(&meta).unwrap().as_bytes())?;
    Ok(Built { id, html, pdf: pdf_path, meta })
}

const CSS: &str = "body{font:14px/1.5 'Pretendard','Noto Sans KR',system-ui,sans-serif;color:#1f2328;background:#fff;max-width:760px;margin:24px auto;padding:0 16px}h1{font-size:22px;margin:0}h2{font-size:15px;margin:22px 0 8px;border-bottom:1px solid #e5e7eb;padding-bottom:4px}.sub{color:#6b7280;margin:4px 0 0}.cards{display:grid;grid-template-columns:repeat(3,1fr);gap:8px;margin-top:16px}.card{border:1px solid #e5e7eb;border-radius:8px;padding:10px}.k{color:#6b7280;font-size:12px}.v{font-size:20px;font-weight:600}.d{font-size:12px;color:#6b7280}.up{color:#15803d}.down{color:#b91c1c}.chart{width:100%;height:auto}.bar{fill:#3b82f6}.heat{fill:#3b82f6}.lbl,.val,.ax{font-size:12px;fill:#374151}.ax{fill:#6b7280;font-size:11px}pre{white-space:pre-wrap;font-size:11px;background:#f6f8fa;padding:6px;border-radius:4px}.appendix{font-size:12px;color:#4b5563}section{break-inside:avoid}@media print{body{margin:0}}.appendix li{margin:4px 0}.appendix pre{display:none}.tg,.tgall{position:absolute;opacity:0;pointer-events:none}.appendix label{cursor:pointer}.sv{color:#2563eb;font-size:11px}.tgl{display:inline-block;margin:4px 0;color:#2563eb;font-size:12px;cursor:pointer}.tg:checked+label+pre,.tgall:checked~ol pre,li:target pre{display:block}li:target{background:#fef9c3}a.src{font-size:11px;color:#2563eb;text-decoration:none;margin-left:4px}h3{font-size:13px;margin:14px 0 4px;color:#374151}.fun ul{margin:4px 0 8px;padding-left:18px}.badges{display:flex;flex-wrap:wrap;gap:6px}.badge{border:1px solid #c7d2fe;background:#eef2ff;color:#3730a3;border-radius:999px;padding:2px 10px;font-size:12px}.next{font-size:12px;color:#4b5563}.prog{display:inline-block;width:120px;height:6px;background:#e5e7eb;border-radius:3px;vertical-align:middle;margin-left:6px}.prog span{display:block;height:6px;background:#6366f1;border-radius:3px}.scene{margin-top:10px;padding:8px 10px;border-left:3px solid #f59e0b;background:#fffbeb;font-size:13px}.strip{display:flex;flex-wrap:wrap;gap:6px;margin:6px 0 10px}.strip span{font-size:12px;border:1px solid #e5e7eb;border-radius:6px;padding:2px 8px}.strip em{font-style:normal;color:#6b7280}.podwrap{text-align:center;margin:6px 0}.podt{font-size:12px;color:#6b7280}.podium{width:330px;height:auto}.podium .p1{fill:#f59e0b}.podium .p2{fill:#9ca3af}.podium .p3{fill:#d97706;opacity:.75}.pn{font-size:12px;text-anchor:middle;fill:#111827;font-weight:600}.pv{font-size:11px;text-anchor:middle;fill:#4b5563}.pr{font-size:22px;text-anchor:middle;fill:#fff;font-weight:700}.cards2{display:grid;grid-template-columns:repeat(2,1fr);gap:8px}.award{position:relative;border:1px solid #e5e7eb;border-radius:10px;padding:10px 10px 8px 46px;break-inside:avoid;background:linear-gradient(180deg,#fffdf5,#fff)}.trophy{position:absolute;left:10px;top:12px;width:26px;height:26px;fill:#f59e0b;stroke:#f59e0b}.at{font-size:12px;color:#92400e;font-weight:600}.aw{font-size:15px}.av{font-size:13px;color:#111827}.ru{font-size:11px;color:#6b7280}.ru span{color:#374151}.field{width:90px;height:18px;display:block;margin-top:4px}.field .f1{fill:#f59e0b}.field .fo{fill:#d1d5db}.af{font-size:11px;color:#9ca3af;margin-top:4px}.pill{display:inline-flex;align-items:center;gap:3px;margin-left:6px;font-size:10px;border:1px solid #e5e7eb;border-radius:999px;padding:0 6px;color:#374151;background:#f9fafb;vertical-align:middle}.pill i{font-style:normal}.pill em{font-style:normal;border-left:1px solid #e5e7eb;padding-left:4px;color:#6b7280}.namers{margin-top:8px;font-size:13px}.nm{display:inline-block;border:1px dashed #a5b4fc;border-radius:6px;padding:0 6px;margin:2px}.muted{color:#6b7280}@media print{.appendix pre{display:block!important;font-size:7px;padding:2px}.appendix label .sv,.tgl,a.src{display:none}.appendix{break-before:page;font-size:9px}}";

fn chrome() -> Option<PathBuf> {
    crate::platform::chrome()
}

fn print_pdf(html: &std::path::Path) -> Res<PathBuf> {
    let bin = chrome().ok_or("PDF: chromium/chrome not found in PATH")?;
    let out = html.with_extension("pdf");
    // a profile of its own per build (concurrent builds must not delete each other's), and a
    // deadline so a hung Chromium cannot stall the caller
    let profile = std::env::temp_dir().join(format!(
        "adash-pdf-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&out);
    let st = crate::platform::timeout(120)
        .arg(bin)
        .args(["--headless=new", "--disable-gpu", "--no-pdf-header-footer", "--no-first-run"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", out.display()))
        .arg(format!("file://{}", html.display()))
        .output()
        .map_err(|e| format!("PDF: {e}"))?;
    let _ = std::fs::remove_dir_all(&profile);
    if !out.exists() {
        return Err(format!("PDF: chrome exited {} without output", st.status));
    }
    Ok(out)
}

pub fn list() -> Value {
    let mut v: Vec<Value> = vec![];
    for p in ["week", "month", "custom"] {
        let Ok(rd) = std::fs::read_dir(dir().join(p)) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "json") {
                if let Some(mut m) = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                    m["hasPdf"] = json!(path.with_extension("pdf").exists());
                    v.push(m);
                }
            }
        }
    }
    v.sort_by_key(|m| std::cmp::Reverse(m["generatedAt"].as_i64().unwrap_or(0)));
    json!({"reports": v})
}

/// id → file path for `ext` (html|pdf); ids are `<period>-<label>` from `list()`.
pub fn path_of(id: &str, ext: &str) -> Option<PathBuf> {
    let (period, label) = id.split_once('-')?;
    if !["week", "month", "custom"].contains(&period) || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let p = dir().join(period).join(format!("{label}.{ext}"));
    p.exists().then_some(p)
}

/// Scheduler (server, every 10 min): weekly report on Monday 09:00 for the previous week,
/// monthly on the 1st 09:00 for the previous month; missed runs catch up after downtime.
pub fn due() -> Vec<(String, String, i64, i64)> {
    let now = Local::now();
    if now.hour() < 9 {
        return vec![];
    }
    let mut out = vec![];
    for (period, prev) in [("week", false), ("month", true)] {
        if let Ok(b) = bounds(period, None, None, prev) {
            if path_of(&format!("{}-{}", b.0, b.1), "json").is_none() {
                out.push(b);
            }
        }
    }
    out
}

/// SQL: milliseconds of `agent_turns`' human gap (the time before the human's message) that the
/// session spent snoozed, from the `state_segments` view.
pub const SNOOZED_OVERLAP: &str = "COALESCE((SELECT SUM(MAX(0, MIN(COALESCE(g.end_ts, agent_turns.start_ts), agent_turns.start_ts) - MAX(g.start_ts, agent_turns.start_ts - agent_turns.human_gap_ms))) FROM state_segments g JOIN sessions s ON s.file = g.identity WHERE s.id = agent_turns.session_id AND g.state = 'snoozed'), 0)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_cover_whole_periods() {
        let (p, l, f, t) = bounds("week", Some("2026-10-19"), Some("2026-10-25"), false).unwrap();
        assert_eq!((p.as_str(), l.as_str()), ("week", "2026-W43"));
        assert_eq!(t - f, 7 * 86_400_000);
        let (_, l, f, t) = bounds("custom", Some("2026-10-01"), Some("2026-10-01"), false).unwrap();
        assert_eq!((l.as_str(), t - f), ("20261001_20261001", 86_400_000));
        let (_, _, f, t) = bounds("week", None, None, false).unwrap();
        assert_eq!(t - f, 7 * 86_400_000);
        assert!(t <= now_ms());
        let (_, _, f, t) = bounds("month", None, None, true).unwrap();
        assert!((28..=31).contains(&((t - f) / 86_400_000)));
        assert!(bounds("custom", None, None, false).is_err());
        assert!(bounds("../../x", Some("2026-10-01"), None, false).is_err(), "period is a folder name");
        assert!(path_of("week-../../etc", "html").is_none());
    }

    #[test]
    fn report_numbers_match_fixture() {
        let dirp = std::env::temp_dir().join(format!("adash-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dirp);
        let db = crate::analytics::open_at(&dirp.join("a.db")).unwrap();
        db.conn.execute("INSERT INTO sessions(file, agent_type, project) VALUES ('/s', 'omp', 'P')", []).unwrap();
        let t0 = 1_800_000_000_000i64;
        for (i, ts) in [t0, t0 + 60_000, t0 + 7_200_000].iter().enumerate() {
            db.conn.execute("INSERT INTO human_messages(rec_key, session_id, ts, project, kind, channel, text) VALUES (?1, 1, ?2, 'P', 'human', 'terminal', '그림 검토 부탁')", params![format!("h{i}"), ts]).unwrap();
        }
        db.conn.execute("INSERT INTO human_messages(rec_key, session_id, ts, project, kind, channel, text) VALUES ('p1', 1, ?1, 'P', 'peer', 'peer', '[m] @b.md 를 읽고 답해줘.')", [t0]).unwrap();
        // two overlapping turns: max parallelism 2
        db.conn.execute("INSERT INTO agent_turns(rec_key, session_id, trigger_kind, start_ts, end_ts, duration_ms, human_gap_ms, project, agent_name, agent_type) VALUES ('h0', 1, 'human', ?1, ?2, 600000, 400000, 'P', 'A', 'omp')", params![t0, t0 + 600_000]).unwrap();
        db.conn.execute("INSERT INTO agent_turns(rec_key, session_id, trigger_kind, start_ts, end_ts, duration_ms, human_gap_ms, project, agent_name, agent_type) VALUES ('h1', 1, 'human', ?1, ?2, 600000, 30000, 'P', 'B', 'omp')", params![t0 + 60_000, t0 + 660_000]).unwrap();
        // not via dir(): render into a temp HOME-independent place by building then reading back
        let r = build(&db, "custom", &format!("test{}", std::process::id()), t0, t0 + 86_400_000, false).unwrap();
        let html = std::fs::read_to_string(&r.html).unwrap();
        assert_eq!(r.meta["headline"]["messages"], 3.0); // peer pointer excluded
        assert_eq!(r.meta["headline"]["turns"], 2.0);
        assert_eq!(r.meta["headline"]["agents"], 1.0); // one top-level session, two turns
        assert_eq!(r.meta["headline"]["activeHours"], 2.0);
        assert_eq!(r.meta["headline"]["agentWorkHours"], 0.3);
        assert!(html.contains("최대 <b>2</b>"));
        assert!(html.contains("<b>0%</b>")); // omp turns, gaps 400 s and 30 s: both within omp/anthropic's 60 min
        assert!(html.contains("검토 스크립트 요청 <b>1</b>"));
        let _ = std::fs::remove_file(&r.html);
        let _ = std::fs::remove_file(r.html.with_extension("json"));
        let _ = std::fs::remove_dir_all(&dirp);
    }
}

#[cfg(test)]
mod snooze_tests {
    #[test]
    fn snoozed_time_is_not_counted_as_waiting_on_me() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY, file TEXT);
             CREATE TABLE agent_turns (session_id INTEGER, start_ts INTEGER, human_gap_ms INTEGER);
             CREATE TABLE state_transitions (identity TEXT, ts INTEGER, state TEXT);
             CREATE VIEW state_segments AS SELECT identity, state, ts AS start_ts, LEAD(ts) OVER (PARTITION BY identity ORDER BY ts) AS end_ts FROM state_transitions;
             INSERT INTO sessions VALUES (1, '/s/a.jsonl');
             -- waiting 0..100, snoozed 20..50, waiting again 50..100; the human answers at 100
             INSERT INTO state_transitions VALUES ('/s/a.jsonl', 0, 'waiting'), ('/s/a.jsonl', 20, 'snoozed'), ('/s/a.jsonl', 50, 'waiting'), ('/s/a.jsonl', 100, 'working');
             INSERT INTO agent_turns VALUES (1, 100, 100);",
        )
        .unwrap();
        let waited: i64 = c.query_row(&format!("SELECT MAX(0, human_gap_ms - {}) FROM agent_turns", super::SNOOZED_OVERLAP), [], |r| r.get(0)).unwrap();
        assert_eq!(waited, 70);
    }
}
