//! "재미있는 기록" for weekly/monthly reports. Every number comes from a query recorded in the
//! report appendix (link "근거"); "my" statistics use `kind = 'human'` messages only.
use crate::report::{delta, esc, fmt_n, Ctx};
use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

const HM: &str = "FROM human_messages WHERE kind = 'human' AND ts >= ?1 AND ts < ?2";

/// Badge thresholds (one table; the report shows achieved badges and progress to the next).
pub const BADGES: &[(&str, &str, &str, f64)] = &[
    ("night", "밤샘러", "03–05시에 보낸 메시지", 1.0),
    ("multi", "멀티태스커", "동시에 일한 에이전트", 6.0),
    ("quick", "빠른 손", "답이 온 뒤 1분 안에 보낸 답장", 20.0),
    ("streak", "끈기", "연속 활동일", 7.0),
    ("review", "검토광", "검토·제안 요청", 20.0),
    ("short", "한 글자 장인", "한두 글자 답", 30.0),
    ("cache", "캐시 지킴이", "5분 안에 답한 비율(%)", 70.0),
];

const STOP: &[&str] = &[
    "the",
    "and",
    "to",
    "of",
    "a",
    "in",
    "is",
    "it",
    "for",
    "that",
    "this",
    "on",
    "with",
    "be",
    "i",
    "you",
    "are",
    "그리고",
    "그",
    "이",
    "저",
    "것",
    "수",
    "좀",
    "더",
    "있는",
    "이거",
    "그거",
    "지금",
    "근데",
    "잘",
    "다",
    "안",
    "를",
    "을",
    "가",
    "는",
    "은",
    "에",
    "도",
    "로",
    "의",
    "에서",
    "하고",
    "그럼",
    "md",
    "파일",
    "이",
    "그냥",
    "또",
];

/// What the user actually wrote: pasted review-comment blocks quote the file ("File: … | Source: … |
/// Line N | Excerpt: | > quote | User comment: \"…\"") — keep only the comments; quoted lines
/// (`>`), code fences and those headers are not the user's words.
pub fn own_words(text: &str) -> String {
    // pasted attachments are not the user's words
    let mut text = text.to_string();
    while let Some(a) = text.find("<attachment") {
        let end = text[a..].find("</attachment>").map(|e| a + e + "</attachment>".len()).unwrap_or(text.len());
        text.replace_range(a..end, "");
    }
    let mut out = vec![];
    let mut fence = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence
            || t.starts_with('>')
            || t.starts_with("File:")
            || t.starts_with("Source:")
            || t.starts_with("Excerpt:")
            || (t.starts_with("Line ") && t[5..].chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            continue;
        }
        if let Some(c) = t.strip_prefix("User comment:") {
            out.push(c.trim().trim_matches('"').to_string());
        } else {
            out.push(t.to_string());
        }
    }
    out.join("\n").trim().to_string()
}

/// Words of a message (lowercase; punctuation, paths and code trimmed).
pub fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| !w.contains('/') && !w.contains('@') && !w.contains('\\'))
        .flat_map(|w| w.split([',', '.', '!', '?', '(', ')', '[', ']', '"', '\'', '`', '“', '”', ':', ';']))
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit() || c == '-'))
        .collect()
}

/// Most frequent 2–3-word expressions (no stopword at either end; at least 3 uses), padded with
/// single words when there are fewer than `k`.
pub fn top_expressions(texts: &[&str], k: usize) -> Vec<(String, usize)> {
    let stop = |w: &str| STOP.contains(&w) || w.chars().count() < 2;
    let mut grams: HashMap<String, usize> = HashMap::new();
    let mut singles: HashMap<String, usize> = HashMap::new();
    for t in texts {
        let ws = words(t);
        let mut seen = std::collections::HashSet::new(); // one count per message
        for n in [2usize, 3] {
            for win in ws.windows(n) {
                if stop(&win[0]) || stop(&win[n - 1]) {
                    continue;
                }
                let g = win.join(" ");
                if seen.insert(g.clone()) {
                    *grams.entry(g).or_default() += 1;
                }
            }
        }
        for w in &ws {
            if !stop(w) && seen.insert(w.clone()) {
                *singles.entry(w.clone()).or_default() += 1;
            }
        }
    }
    let mut g: Vec<(String, usize)> = grams.into_iter().filter(|(_, n)| *n >= 3).collect();
    g.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // a 2-gram that only ever appears inside a kept 3-gram is redundant
    let mut out: Vec<(String, usize)> = vec![];
    for (e, n) in g {
        if out.iter().any(|(o, m)| *m == n && o.contains(&e)) {
            continue;
        }
        out.push((e, n));
        if out.len() == k {
            return out;
        }
    }
    let mut s: Vec<(String, usize)> = singles.into_iter().collect();
    s.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (w, n) in s {
        if out.len() == k {
            break;
        }
        if !out.iter().any(|(o, _)| o.split(' ').any(|x| x == w)) {
            out.push((w, n));
        }
    }
    out
}

/// Longest run of consecutive calendar days.
pub fn longest_streak(days: &BTreeSet<NaiveDate>) -> usize {
    let (mut best, mut run, mut prev): (usize, usize, Option<NaiveDate>) = (0, 0, None);
    for d in days {
        run = if prev.and_then(|p| p.succ_opt()) == Some(*d) { run + 1 } else { 1 };
        best = best.max(run);
        prev = Some(*d);
    }
    best
}

/// Longest chain of messages with gaps under `gap_ms`: (start, end, messages).
pub fn focus_span(ts: &[i64], gap_ms: i64) -> Option<(i64, i64, usize)> {
    let mut best: Option<(i64, i64, usize)> = None;
    let mut i = 0;
    while i < ts.len() {
        let mut j = i;
        while j + 1 < ts.len() && ts[j + 1] - ts[j] < gap_ms {
            j += 1;
        }
        if best.is_none_or(|b| ts[j] - ts[i] > b.1 - b.0) {
            best = Some((ts[i], ts[j], j - i + 1));
        }
        i = j + 1;
    }
    best.filter(|b| b.2 > 1)
}

fn hm(min: f64) -> String {
    let m = (min.round() as i64).rem_euclid(1440);
    format!("{:02}:{:02}", m / 60, m % 60)
}

fn at(ts: i64, f: &str) -> String {
    Local.timestamp_millis_opt(ts).single().map(|t| t.format(f).to_string()).unwrap_or_default()
}

fn first_line(t: &str, n: usize) -> String {
    crate::util::truncate(t.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim(), n)
}

struct Speech {
    n: usize,
    thanks: usize,
    question: usize,
    short: usize,
    correction: usize,
    night: usize,
}

fn speech(texts: &[(i64, String)]) -> Speech {
    let mut s = Speech { n: texts.len(), thanks: 0, question: 0, short: 0, correction: 0, night: 0 };
    for (ts, t) in texts {
        let t = t.trim();
        if t.contains("고마") || t.contains("감사") || t.to_lowercase().contains("thank") {
            s.thanks += 1;
        }
        if t.ends_with('?') || t.ends_with('？') {
            s.question += 1;
        }
        if t.chars().count() <= 2 && !t.starts_with('/') {
            s.short += 1;
        }
        if ["아니", "다시", "그게 아니라", "취소", "말고"].iter().any(|k| t.contains(k)) {
            s.correction += 1;
        }
        if Local.timestamp_millis_opt(*ts).single().is_some_and(|d| d.hour() < 5) {
            s.night += 1;
        }
    }
    s
}

fn pct(a: usize, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        a as f64 / n as f64 * 100.0
    }
}

/// Peak concurrency (count, at) and time-weighted mean while at least one agent worked.
pub fn concurrency(intervals: &[(i64, i64)]) -> (i32, i64, f64) {
    let mut ev: Vec<(i64, i32)> = intervals.iter().flat_map(|(s, e)| [(*s, 1), (*e.max(s), -1)]).collect();
    ev.sort();
    let (mut cur, mut max, mut max_at, mut busy, mut weighted, mut last) = (0, 0, 0, 0i64, 0i64, 0i64);
    for (t, d) in ev {
        if cur > 0 {
            busy += t - last;
            weighted += (t - last) * cur as i64;
        }
        cur += d;
        if cur > max {
            max = cur;
            max_at = t;
        }
        last = t;
    }
    (max, max_at, if busy > 0 { weighted as f64 / busy as f64 } else { 0.0 })
}

pub(crate) fn section(c: &mut Ctx, prev: (i64, i64)) -> String {
    let mut h = String::from("<section class=fun><h2>재미있는 기록</h2>");
    let li = |h: &mut String, txt: String| {
        let _ = write!(h, "<li>{txt}</li>");
    };

    // ---- my messages
    let msg_sql = format!("SELECT ts, text, session_id {HM} AND is_slash = 0 ORDER BY ts");
    let rows = c.table("내 메시지(사람이 보낸 것만, 슬래시 명령 제외; 말버릇은 붙여 넣은 리뷰 주석의 인용·머리줄을 빼고 내가 쓴 말만)", &msg_sql);
    let src_msgs = c.src(None);
    let msgs: Vec<(i64, String, i64)> = rows
        .iter()
        .map(|r| (r[0].as_i64().unwrap_or(0), own_words(r[1].as_str().unwrap_or("")), r[2].as_i64().unwrap_or(0)))
        .filter(|m| !m.1.is_empty())
        .collect();
    let prev_rows = c.table_at(&msg_sql, prev.0, prev.1);
    let prev_msgs: Vec<(i64, String)> =
        prev_rows.iter().map(|r| (r[0].as_i64().unwrap_or(0), own_words(r[1].as_str().unwrap_or("")))).filter(|m| !m.1.is_empty()).collect();
    if msgs.is_empty() {
        h.push_str("<p class=muted>이 기간에는 보낸 메시지가 없습니다.</p></section>");
        return h;
    }
    let local: Vec<chrono::DateTime<Local>> = msgs.iter().filter_map(|m| Local.timestamp_millis_opt(m.0).single()).collect();
    let tss: Vec<i64> = msgs.iter().map(|m| m.0).collect();

    // ---- 리듬
    h.push_str("<h3>리듬</h3><ul>");
    let mut by_day: BTreeMap<NaiveDate, (u32, u32)> = BTreeMap::new();
    for t in &local {
        // a day runs 05:00–05:00 so a late night belongs to the day before
        let shifted = *t - chrono::Duration::hours(5);
        let m = shifted.hour() * 60 + shifted.minute();
        let e = by_day.entry(shifted.date_naive()).or_insert((m, m));
        e.0 = e.0.min(m);
        e.1 = e.1.max(m);
    }
    let avg = |f: &dyn Fn(&(u32, u32)) -> u32| by_day.values().map(|v| f(v) as f64).sum::<f64>() / by_day.len() as f64 + 300.0;
    li(&mut h, format!("출근 평균 <b>{}</b> · 퇴근 평균 <b>{}</b> (하루는 05시에 바뀝니다){src_msgs}", hm(avg(&|v| v.0)), hm(avg(&|v| v.1))));
    let sp = speech(&msgs.iter().map(|m| (m.0, m.1.clone())).collect::<Vec<_>>());
    let spp = speech(&prev_msgs);
    let latest = msgs.iter().max_by_key(|m| Local.timestamp_millis_opt(m.0).single().map(|t| ((t.hour() + 19) % 24) * 60 + t.minute()).unwrap_or(0));
    if let Some(l) = latest {
        li(
            &mut h,
            format!(
                "야행성 지수 <b>{:.0}%</b> (00–05시) {} · 가장 늦은 메시지 <b>{}</b> “{}”{src_msgs}",
                pct(sp.night, sp.n),
                delta(pct(sp.night, sp.n), pct(spp.night, spp.n)),
                at(l.0, "%m-%d %H:%M"),
                esc(&first_line(&l.1, 20))
            ),
        );
    }
    if let Some(e) = msgs
        .iter()
        .filter(|m| Local.timestamp_millis_opt(m.0).single().is_some_and(|t| t.hour() >= 5))
        .min_by_key(|m| Local.timestamp_millis_opt(m.0).single().map(|t| t.hour() * 60 + t.minute()).unwrap_or(0))
    {
        li(&mut h, format!("가장 이른 아침 <b>{}</b> “{}”{src_msgs}", at(e.0, "%m-%d %H:%M"), esc(&first_line(&e.1, 20))));
    }
    let mut wd = [0usize; 7];
    let mut hr = [0usize; 24];
    for t in &local {
        wd[t.weekday().num_days_from_monday() as usize] += 1;
        hr[t.hour() as usize] += 1;
    }
    let days = ["월", "화", "수", "목", "금", "토", "일"];
    let (bw, bwn) = wd.iter().enumerate().max_by_key(|x| x.1).map(|(i, n)| (i, *n)).unwrap();
    let (bh, bhn) = hr.iter().enumerate().max_by_key(|x| x.1).map(|(i, n)| (i, *n)).unwrap();
    li(
        &mut h,
        format!(
            "가장 바쁜 요일 <b>{}요일</b>({}건) · 시간 <b>{bh}시</b>({}건) · 주말 비율 <b>{:.0}%</b>{src_msgs}",
            days[bw],
            fmt_n(bwn as f64),
            fmt_n(bhn as f64),
            pct(wd[5] + wd[6], local.len())
        ),
    );
    let day_set: BTreeSet<NaiveDate> = local.iter().map(|t| t.date_naive()).collect();
    let streak = longest_streak(&day_set);
    let focus = focus_span(&tss, 20 * 60_000);
    li(
        &mut h,
        format!(
            "최장 연속 활동 <b>{streak}일</b>{}{src_msgs}",
            focus
                .map(|(s, e, n)| format!(
                    " · 쉬지 않은 최장 집중 <b>{:.1}시간</b> ({}–{}, 메시지 {n}건, 간격 20분 미만)",
                    (e - s) as f64 / 3.6e6,
                    at(s, "%m-%d %H:%M"),
                    at(e, "%H:%M")
                ))
                .unwrap_or_default()
        ),
    );
    h.push_str("</ul>");

    // ---- 말버릇
    h.push_str("<h3>말버릇</h3><ul>");
    let texts: Vec<&str> = msgs.iter().map(|m| m.1.as_str()).collect();
    let top = top_expressions(&texts, 5);
    if !top.is_empty() {
        li(&mut h, format!("자주 쓴 표현 {}{src_msgs}", top.iter().map(|(e, n)| format!("<b>“{}”</b> {n}", esc(e))).collect::<Vec<_>>().join(" · ")));
    }
    li(
        &mut h,
        format!(
            "고마워·감사 <b>{}</b>회 {} · 질문형 지수 <b>{:.0}%</b> {} · 한두 글자 답 <b>{}</b>회 {}{src_msgs}",
            sp.thanks,
            delta(sp.thanks as f64, spp.thanks as f64),
            pct(sp.question, sp.n),
            delta(pct(sp.question, sp.n), pct(spp.question, spp.n)),
            sp.short,
            delta(sp.short as f64, spp.short as f64)
        ),
    );
    let long_ = msgs.iter().max_by_key(|m| m.1.chars().count());
    let short_ = msgs.iter().filter(|m| m.1.trim().chars().count() >= 3).min_by_key(|m| m.1.trim().chars().count());
    if let (Some(l), Some(s)) = (long_, short_) {
        li(
            &mut h,
            format!(
                "가장 짧은 지시 <b>{}자</b> “{}” · 가장 긴 지시 <b>{}자</b> “{}…”{src_msgs}",
                s.1.trim().chars().count(),
                esc(&first_line(&s.1, 30)),
                fmt_n(l.1.chars().count() as f64),
                esc(&first_line(&l.1, 30))
            ),
        );
    }
    let lang = c.table("언어 비율", &format!("SELECT lang, COUNT(*) {HM} GROUP BY lang"));
    let src_lang = c.src(None);
    let ln: HashMap<String, f64> = lang.iter().map(|r| (r[0].as_str().unwrap_or("").to_string(), r[1].as_f64().unwrap_or(0.0))).collect();
    let tot: f64 = ln.values().sum::<f64>().max(1.0);
    li(
        &mut h,
        format!(
            "한국어 <b>{:.0}%</b> · 영어 {:.0}% · 섞어 쓰기 {:.0}%{src_lang}",
            ln.get("ko").unwrap_or(&0.0) / tot * 100.0,
            ln.get("en").unwrap_or(&0.0) / tot * 100.0,
            ln.get("mixed").unwrap_or(&0.0) / tot * 100.0
        ),
    );
    let intr = c.table("중단한 턴(인터럽트)", "SELECT COUNT(*) FROM agent_turns WHERE interrupted = 1 AND start_ts >= ?1 AND start_ts < ?2");
    let src_int = c.src(None);
    li(
        &mut h,
        format!(
            "방향 전환 지수: 정정 표현(아니·다시·그게 아니라·취소·말고) <b>{}</b>회 · 중단 <b>{}</b>회{src_msgs}{src_int}",
            sp.correction,
            intr.first().and_then(|r| r[0].as_i64()).unwrap_or(0)
        ),
    );
    h.push_str("</ul>");

    // ---- 검토 리그 (thread files)
    c.note(
        "검토 리그: 스레드 파일(~/.local/share/adash/threads/*/thread.json, reply.md 첫 줄의 '수락'/'수정 필요'), magi 실행 기록",
        "-- 파일 기반: created_at ∈ [?1, ?2), from != 'human'",
    );
    let src_thr = c.src(None);
    let ts: Vec<crate::threads::Thread> =
        crate::threads::load_all().into_iter().filter(|t| t.from != "human" && t.created_at >= c.from && t.created_at < c.to).collect();
    let magi = crate::magi::RunState::load_all().iter().filter(|r| r.created_at >= c.from && r.created_at < c.to).count();
    if !ts.is_empty() || magi > 0 {
        h.push_str("<h3>검토 리그</h3><ul>");
        let mut pairs: HashMap<(String, String), usize> = HashMap::new();
        let mut strict: HashMap<String, (usize, usize)> = HashMap::new();
        let mut fastest: Option<(i64, &crate::threads::Thread)> = None;
        for t in &ts {
            *pairs.entry((t.from.clone(), t.to.clone())).or_default() += 1;
            if let Some(first) = t.reply_text().and_then(|r| r.lines().find(|l| !l.trim().is_empty()).map(|l| l.to_string())) {
                let e = strict.entry(t.to.clone()).or_default();
                if first.contains("수정 필요") {
                    e.0 += 1;
                } else if first.contains("수락") {
                    e.1 += 1;
                }
            }
            if let Some(a) = t.events.iter().find(|e| e.kind == "answered").map(|e| e.at - t.created_at).filter(|d| *d > 0) {
                if fastest.is_none_or(|f| a < f.0) {
                    fastest = Some((a, t));
                }
            }
        }
        if let Some(((f, t), n)) = pairs.iter().max_by_key(|x| x.1) {
            li(&mut h, format!("단골 짝꿍 <b>{} → {}</b> — 요청 {n}건{src_thr}", esc(f), esc(t)));
        }
        if let Some((who, (fix, ok))) = strict
            .iter()
            .filter(|(_, v)| v.0 + v.1 >= 2)
            .max_by(|a, b| (a.1 .0 as f64 / (a.1 .0 + a.1 .1) as f64).total_cmp(&(b.1 .0 as f64 / (b.1 .0 + b.1 .1) as f64)))
        {
            li(&mut h, format!("가장 엄한 검토자 <b>{}</b> — 수정 필요 {fix} : 수락 {ok}{src_thr}", esc(who)));
        }
        if let Some((d, t)) = fastest {
            li(&mut h, format!("가장 빠른 검토 답 <b>{}</b> — {:.1}분 ({} → {}){src_thr}", esc(&t.to), d as f64 / 60_000.0, esc(&t.from), esc(&t.to)));
        }
        li(&mut h, format!("검토·제안 요청 <b>{}</b>건 · magi 실행 <b>{magi}</b>회{src_thr}", ts.len()));
        h.push_str("</ul>");
    }

    // ---- 멀티태스킹
    h.push_str("<h3>멀티태스킹</h3><ul>");
    let iv = c.table(
        "턴 구간(동시 작업 계산)",
        "SELECT start_ts, COALESCE(end_ts, start_ts + COALESCE(duration_ms, 0)) FROM agent_turns WHERE start_ts >= ?1 AND start_ts < ?2 AND duration_ms > 0",
    );
    let src_iv = c.src(None);
    let intervals: Vec<(i64, i64)> = iv.iter().map(|r| (r[0].as_i64().unwrap_or(0), r[1].as_i64().unwrap_or(0).min(c.to))).collect();
    let (maxc, max_at, mean) = concurrency(&intervals);
    let mut touched: BTreeMap<NaiveDate, BTreeSet<i64>> = BTreeMap::new();
    for m in &msgs {
        if let Some(t) = Local.timestamp_millis_opt(m.0).single() {
            touched.entry(t.date_naive()).or_default().insert(m.2);
        }
    }
    let busiest_day = touched.iter().max_by_key(|x| x.1.len());
    li(
        &mut h,
        format!(
            "동시 작업 최고 <b>{maxc}</b>개 ({}) · 평균 <b>{mean:.1}</b>개{src_iv}{}",
            at(max_at, "%m-%d %H:%M"),
            busiest_day.map(|(d, s)| format!(" · 하루에 말 건 에이전트 최고 <b>{}</b>개 ({}){src_msgs}", s.len(), d.format("%m-%d"))).unwrap_or_default()
        ),
    );
    h.push_str("</ul>");

    // ---- 도구와 파일
    h.push_str("<h3>도구와 파일</h3><ul>");
    let top_list = |c: &mut Ctx, label: &str, sql: &str| -> (Vec<(String, i64)>, String) {
        let r = c.table(label, sql);
        (r.iter().map(|x| (x[0].as_str().unwrap_or("").to_string(), x[1].as_i64().unwrap_or(0))).collect(), c.src(None))
    };
    let short_path = |p: &str| {
        let home = crate::util::home().to_string_lossy().into_owned();
        let p = p.replacen(&home, "~", 1);
        let parts: Vec<&str> = p.rsplitn(3, '/').collect();
        if parts.len() == 3 {
            format!("…/{}/{}", parts[1], parts[0])
        } else {
            p
        }
    };
    let (read, s1) = top_list(c, "가장 많이 읽힌 파일", "SELECT substr(target, 6), COUNT(*) FROM tool_calls WHERE target LIKE 'file:%' AND instr(target, char(10)) = 0 AND lower(tool) IN ('read', 'read_file', 'view') AND ts >= ?1 AND ts < ?2 GROUP BY 1 ORDER BY 2 DESC LIMIT 5");
    if !read.is_empty() {
        li(
            &mut h,
            format!(
                "가장 많이 읽힌 파일 {}{s1}",
                read.iter().map(|(p, n)| format!("<code>{}</code> {n}", esc(&short_path(p)))).collect::<Vec<_>>().join(" · ")
            ),
        );
    }
    let er = c.table("가장 많이 고친 파일(한 패치가 여러 파일이면 파일마다 1)", "SELECT target, COUNT(*) FROM tool_calls WHERE target LIKE 'file:%' AND lower(tool) NOT IN ('read', 'read_file', 'view') AND ts >= ?1 AND ts < ?2 GROUP BY 1");
    let s2 = c.src(None);
    let mut ec: HashMap<String, i64> = HashMap::new();
    for r in &er {
        for p in r[0].as_str().unwrap_or("").lines().filter_map(|l| l.strip_prefix("file:")) {
            *ec.entry(p.to_string()).or_default() += r[1].as_i64().unwrap_or(0);
        }
    }
    let mut edit: Vec<(String, i64)> = ec.into_iter().collect();
    edit.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    edit.truncate(5);
    if !edit.is_empty() {
        li(
            &mut h,
            format!(
                "가장 많이 고친 파일 {}{s2}",
                edit.iter().map(|(p, n)| format!("<code>{}</code> {n}", esc(&short_path(p)))).collect::<Vec<_>>().join(" · ")
            ),
        );
    }
    let (cmds, s3) = top_list(
        c,
        "가장 많이 쓴 셸 명령(첫 단어)",
        "SELECT substr(target, 5), COUNT(*) FROM tool_calls WHERE target LIKE 'cmd:%' AND ts >= ?1 AND ts < ?2 GROUP BY 1 ORDER BY 2 DESC LIMIT 5",
    );
    if !cmds.is_empty() {
        li(
            &mut h,
            format!(
                "자주 쓴 셸 명령 {}{s3}",
                cmds.iter().map(|(p, n)| format!("<code>{}</code> {}", esc(p), fmt_n(*n as f64))).collect::<Vec<_>>().join(" · ")
            ),
        );
    }
    let img = c.table("붙여 넣은 이미지", &format!("SELECT SUM(attachments) {HM}"));
    let s4 = c.src(None);
    li(&mut h, format!("붙여 넣은 이미지 <b>{}</b>장{s4}", img.first().and_then(|r| r[0].as_i64()).unwrap_or(0)));
    h.push_str("</ul>");

    // ---- 비용 감각
    h.push_str("<h3>비용 감각</h3><ul>");
    let u = c.table("토큰(입력·출력·캐시 읽기·캐시 쓰기)·비용 합계", "SELECT SUM(COALESCE(input_tokens,0)), SUM(COALESCE(output_tokens,0)), SUM(COALESCE(cache_read_tokens,0)), SUM(COALESCE(cache_write_tokens,0)), SUM(cost) FROM usage_events WHERE ts >= ?1 AND ts < ?2");
    let s5 = c.src(None);
    let g = |i: usize| u.first().and_then(|r| r[i].as_f64()).unwrap_or(0.0);
    let (inp, out, cr, cw, cost) = (g(0), g(1), g(2), g(3), g(4));
    // books count what was actually written/read anew (input + output), not cache re-reads
    let fresh = inp + out;
    li(&mut h, format!("토큰: 입력 {} · 출력 {} · 캐시 읽기 {} · 캐시 쓰기 {} (캐시 읽기가 대부분입니다){s5}", fmt_n(inp), fmt_n(out), fmt_n(cr), fmt_n(cw)));
    li(
        &mut h,
        format!(
            "입력+출력 {} 토큰 = 책 <b>{}</b>권 (캐시 읽기 제외, 1권 ≈ 10만 단어 ≈ 13만 토큰 가정) · 비용 ${cost:.2} = 커피 <b>{}</b>잔 (1잔 $5 가정){s5}",
            fmt_n(fresh),
            fmt_n((fresh / 130_000.0).round()),
            fmt_n((cost / 5.0).round())
        ),
    );
    let lost = c.table("캐시 만료 뒤 턴의 캐시 쓰기 비용 추정", "SELECT SUM(cost * cache_write_tokens * 1.0 / NULLIF(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens, 0)) FROM agent_turns WHERE start_ts >= ?1 AND start_ts < ?2 AND human_gap_ms > 300000 AND cache_write_tokens > 0");
    let s6 = c.src(None);
    if let Some(l) = lost.first().and_then(|r| r[0].as_f64()).filter(|l| *l > 0.0) {
        li(&mut h, format!("캐시를 놓쳐 생긴 추정 추가 비용 <b>${l:.2}</b> (커피 {}잔){s6}", fmt_n((l / 5.0).round())));
    }
    h.push_str("</ul>");

    // ---- 업적 배지
    let gaps = c.table(
        "내 답장 간격(직전 턴 종료→다음 메시지)",
        "SELECT human_gap_ms FROM agent_turns WHERE trigger_kind = 'human' AND human_gap_ms IS NOT NULL AND start_ts >= ?1 AND start_ts < ?2",
    );
    let s7 = c.src(None);
    let g: Vec<i64> = gaps.iter().filter_map(|r| r[0].as_i64()).collect();
    let night3 = local.iter().filter(|t| (3..5).contains(&t.hour())).count() as f64;
    let metric = |id: &str| -> f64 {
        match id {
            "night" => night3,
            "multi" => maxc as f64,
            "quick" => g.iter().filter(|x| **x <= 60_000).count() as f64,
            "streak" => streak as f64,
            "review" => ts.len() as f64,
            "short" => sp.short as f64,
            "cache" => pct(g.iter().filter(|x| **x <= 300_000).count(), g.len()),
            _ => 0.0,
        }
    };
    h.push_str("<h3>업적</h3><div class=badges>");
    let mut next: Option<(f64, String)> = None;
    for (id, name, what, th) in BADGES {
        let v = metric(id);
        if v >= *th {
            let _ = write!(h, "<span class=badge title=\"{what} {} (기준 {})\"><b>{name}</b> {what} {}</span>", fmt_n(v), fmt_n(*th), fmt_n(v));
        } else {
            let p = v / th;
            if next.as_ref().is_none_or(|n| p > n.0) {
                next = Some((p, format!("다음 배지 <b>{name}</b>: {what} {} / {}", fmt_n(v), fmt_n(*th))));
            }
        }
    }
    h.push_str("</div>");
    if let Some((p, t)) = next {
        let _ = write!(h, "<p class=next>{t} <span class=prog><span style=\"width:{:.0}%\"></span></span>{src_msgs}{s7}</p>", (p * 100.0).min(100.0));
    }

    // ---- 이번 기간의 한 장면
    let mut hours: BTreeMap<i64, (usize, BTreeSet<i64>)> = BTreeMap::new();
    for m in &msgs {
        let e = hours.entry(m.0 / 3_600_000).or_default();
        e.0 += 1;
        e.1.insert(m.2);
    }
    if let Some((hk, (n, ag))) = hours.iter().max_by_key(|x| x.1 .0) {
        let s = hk * 3_600_000;
        let _ = write!(
            h,
            "<p class=scene>이번 기간의 한 장면: 가장 바빴던 한 시간 <b>{}–{}</b>, 메시지 {n}건, 에이전트 {}개{src_msgs}</p>",
            at(s, "%m/%d %H:%M"),
            at(s + 3_600_000, "%H:%M"),
            ag.len()
        );
    }
    h.push_str("</section>");
    h
}

// ======================= 에이전트 시상식 =======================

struct Award {
    title: &'static str,
    formula: String,
    /// best first: (name, value, harness, model)
    field: Vec<(String, f64, String, String)>,
    value: fn(f64) -> String,
    src: String,
}

/// Harness/model pill like the dashboard's: shape monogram + model family.
fn pill(harness: &str, model: &str) -> String {
    let (shape, short) = match harness {
        "omp" => ("●", "omp"),
        "claude" => ("■", "cc"),
        "codex" => ("▲", "cdx"),
        "antigravity" => ("◆", "agy"),
        "" => ("", ""),
        _ => ("○", harness),
    };
    if short.is_empty() {
        return String::new();
    }
    let m = model.to_lowercase();
    let fam = ["opus", "sonnet", "haiku", "gpt", "gemini", "glm", "argon", "astra", "luna", "sol"].into_iter().find(|f| m.contains(f)).map(|f| match f {
        "sonnet" => "son",
        "haiku" => "hku",
        "gemini" | "argon" => "gem",
        "astra" | "luna" | "sol" => "gpt",
        x => x,
    });
    format!("<span class=pill><i>{shape}</i>{short}{}</span>", fam.map(|f| format!("<em>{f}</em>")).unwrap_or_default())
}

const TROPHY: &str = "<svg viewBox='0 0 24 24' class=trophy aria-hidden=true><path d='M7 3h10v4a5 5 0 0 1-10 0z'/><path d='M5 4H3v2a4 4 0 0 0 4 4M19 4h2v2a4 4 0 0 1-4 4' fill=none stroke-width=1.6/><rect x=11 y=12 width=2 height=4/><rect x=8 y=17 width=8 height=3 rx=1/></svg>";

fn card(a: &Award) -> String {
    let Some(w) = a.field.first() else { return String::new() };
    let max = a.field.iter().map(|x| x.1).fold(0.0, f64::max).max(1e-9);
    let n = a.field.len().min(8);
    let mut spark = format!("<svg viewBox='0 0 {} 18' class=field>", n * 10);
    for (i, x) in a.field.iter().take(n).enumerate() {
        let hgt = (x.1 / max * 16.0).max(1.0);
        let _ = write!(
            spark,
            "<rect x={} y={:.1} width=7 height={hgt:.1} rx=1 class='{}'><title>{}: {}</title></rect>",
            i * 10,
            18.0 - hgt,
            if i == 0 { "f1" } else { "fo" },
            esc(&x.0),
            (a.value)(x.1)
        );
    }
    spark.push_str("</svg>");
    let runners: String = a
        .field
        .iter()
        .skip(1)
        .take(2)
        .enumerate()
        .map(|(i, x)| format!("<div class=ru>{}위 {} <span>{}</span></div>", i + 2, esc(&x.0), (a.value)(x.1)))
        .collect();
    format!(
        "<div class=award>{TROPHY}<div class=at>{}</div><div class=aw><b>{}</b>{}</div><div class=av>{}</div>{runners}{spark}<div class=af>{}{}</div></div>",
        a.title,
        esc(&w.0),
        pill(&w.2, &w.3),
        (a.value)(w.1),
        esc(&a.formula),
        a.src
    )
}

fn podium(a: &Award) -> String {
    if a.field.len() < 2 {
        return String::new();
    }
    let order = [1usize, 0, 2]; // 2nd, 1st, 3rd from the left
    let hts = [70.0, 100.0, 50.0];
    let mut s = String::from("<svg viewBox='0 0 330 150' class=podium>");
    for (slot, &i) in order.iter().enumerate() {
        let Some(x) = a.field.get(i) else { continue };
        let h = hts[slot];
        let left = slot as f64 * 110.0;
        let _ = write!(
            s,
            "<rect x={left} y={} width=104 height={h} rx=4 class='p{}'/><text x={} y={} class=pn>{}</text><text x={} y={} class=pv>{}</text><text x={} y={} class=pr>{}</text>",
            150.0 - h,
            i + 1,
            left + 52.0,
            150.0 - h - 22.0,
            esc(&crate::util::truncate(&x.0, 12)),
            left + 52.0,
            150.0 - h - 8.0,
            (a.value)(x.1),
            left + 52.0,
            150.0 - h + 22.0,
            i + 1
        );
    }
    s.push_str("</svg>");
    s
}

fn hours(v: f64) -> String {
    format!("{v:.1}시간")
}
fn mins(v: f64) -> String {
    format!("{v:.1}분")
}
fn count(v: f64) -> String {
    fmt_n(v).to_string()
}
fn chars(v: f64) -> String {
    format!("{}자", fmt_n(v))
}
fn pct_s(v: f64) -> String {
    format!("{v:.0}%")
}
fn score(v: f64) -> String {
    format!("{v:.1}점")
}

/// Award candidates: agents I actually talked to (≥ 3 of my messages in the period), outside
/// test sessions (launched with `test: true`).
const TEST_SESSIONS: &str = "SELECT id FROM sessions WHERE is_test = 1";
fn eligible_sql() -> String {
    format!("SELECT m.agent_name FROM human_messages m WHERE m.kind = 'human' AND m.ts >= ?1 AND m.ts < ?2 AND m.agent_name IS NOT NULL AND m.session_id NOT IN ({TEST_SESSIONS}) GROUP BY m.agent_name HAVING COUNT(*) >= 3")
}

pub(crate) fn awards(c: &mut Ctx) -> String {
    let elig_rows = c.table("수상 후보: 내 메시지 3건 이상 받은 에이전트(테스트 세션 제외)", &eligible_sql());
    let eligible: std::collections::HashSet<String> = elig_rows.iter().filter_map(|r| r[0].as_str().map(String::from)).collect();
    // name, value, harness, model; harness/model from the agent's latest turn in the period
    const WHO: &str = "(SELECT t2.agent_type FROM agent_turns t2 WHERE t2.agent_name = x.agent_name ORDER BY t2.start_ts DESC LIMIT 1), (SELECT t2.model FROM agent_turns t2 WHERE t2.agent_name = x.agent_name ORDER BY t2.start_ts DESC LIMIT 1)";
    let field = |c: &mut Ctx, label: &str, sql: String| -> (Vec<(String, f64, String, String)>, String) {
        let r = c.table(label, &sql);
        let v = r
            .iter()
            .filter_map(|x| {
                Some((
                    x[0].as_str()?.to_string(),
                    x[1].as_f64().unwrap_or(0.0),
                    x[2].as_str().unwrap_or("").to_string(),
                    x[3].as_str().unwrap_or("").to_string(),
                ))
            })
            .collect();
        (v, c.src(None))
    };
    let mut list: Vec<Award> = vec![];
    let base = format!("FROM agent_turns x WHERE x.agent_name IS NOT NULL AND x.start_ts >= ?1 AND x.start_ts < ?2 AND x.agent_name IN ({}) AND x.session_id NOT IN ({TEST_SESSIONS})", eligible_sql());
    let base = base.as_str();
    let push = |list: &mut Vec<Award>, title: &'static str, formula: &str, f: (Vec<(String, f64, String, String)>, String), value: fn(f64) -> String| {
        if !f.0.is_empty() {
            list.push(Award { title, formula: formula.into(), field: f.0, value, src: f.1 });
        }
    };
    let f = field(
        c,
        "최다 근무상: 에이전트별 작업 시간",
        format!("SELECT x.agent_name, SUM(x.duration_ms) / 3600000.0 AS v, {WHO} {base} GROUP BY 1 ORDER BY v DESC LIMIT 8"),
    );
    push(&mut list, "최다 근무상", "턴 소요 시간 합(h)", f, hours);
    // 번개상: median turn duration (≥5 turns) — computed in Rust from the turn list
    let r = c
        .table("번개상: 턴 소요 시간 목록(에이전트별 중앙값, 턴 5개 이상)", &format!("SELECT x.agent_name, x.duration_ms, {WHO} {base} AND x.duration_ms > 0"));
    let src = c.src(None);
    let mut per: HashMap<String, (Vec<i64>, String, String)> = HashMap::new();
    for x in &r {
        let e = per.entry(x[0].as_str().unwrap_or("").to_string()).or_insert((vec![], x[2].as_str().unwrap_or("").into(), x[3].as_str().unwrap_or("").into()));
        e.0.push(x[1].as_i64().unwrap_or(0));
    }
    let mut med: Vec<(String, f64, String, String)> = per
        .into_iter()
        .filter(|(_, v)| v.0.len() >= 5)
        .map(|(n, mut v)| {
            v.0.sort_unstable();
            (n, v.0[v.0.len() / 2] as f64 / 60_000.0, v.1, v.2)
        })
        .collect();
    med.sort_by(|a, b| a.1.total_cmp(&b.1));
    push(&mut list, "번개상", "턴 소요 시간 중앙값이 가장 짧은 에이전트(턴 5개 이상)", (med, src), mins);
    let f = field(
        c,
        "마라톤상: 가장 긴 단일 턴(분)",
        format!("SELECT x.agent_name, MAX(x.duration_ms) / 60000.0 AS v, {WHO} {base} GROUP BY 1 ORDER BY v DESC LIMIT 8"),
    );
    push(&mut list, "마라톤상", "가장 긴 단일 턴(분)", f, mins);
    let f = field(c, "수다상: 답변 글자 수", format!("SELECT x.agent_name, SUM(x.assistant_chars) AS v, {WHO} {base} GROUP BY 1 ORDER BY v DESC LIMIT 8"));
    push(&mut list, "수다상", "보이는 답변 글자 수 합(도구·생각 제외)", f, chars);
    let f = field(c, "공구왕: 한 턴의 도구 호출 최다", format!("SELECT x.agent_name, MAX(x.tool_calls) AS v, {WHO} {base} GROUP BY 1 ORDER BY v DESC LIMIT 8"));
    push(&mut list, "공구왕", "한 턴에서 부른 도구 수 최댓값", f, count);
    let f = field(
        c,
        "호기심상: ask 질문 수",
        "SELECT x.agent_name, COUNT(*) AS v, (SELECT t2.agent_type FROM agent_turns t2 WHERE t2.agent_name = x.agent_name ORDER BY t2.start_ts DESC LIMIT 1), (SELECT t2.model FROM agent_turns t2 WHERE t2.agent_name = x.agent_name ORDER BY t2.start_ts DESC LIMIT 1) FROM (SELECT (SELECT t.agent_name FROM agent_turns t WHERE t.session_id = a.session_id AND t.agent_name IS NOT NULL ORDER BY t.start_ts DESC LIMIT 1) AS agent_name FROM asks a WHERE a.asked_at >= ?1 AND a.asked_at < ?2) x WHERE x.agent_name IS NOT NULL AND x.agent_name IN ({}) GROUP BY 1 ORDER BY v DESC LIMIT 8".replace("{}", &eligible_sql()),
    );
    push(&mut list, "호기심상", "나에게 한 질문(ask) 수", f, count);
    let f = field(
        c,
        "인내상: 내 답장을 기다린 최장 시간",
        format!("SELECT x.agent_name, MAX(x.human_gap_ms) / 3600000.0 AS v, {WHO} {base} AND x.trigger_kind = 'human' GROUP BY 1 ORDER BY v DESC LIMIT 8"),
    );
    push(&mut list, "인내상", "직전 답이 끝난 뒤 내 다음 메시지까지 가장 오래 기다린 시간", f, hours);
    // 신인상: first turn ever falls in the period, most work
    let f = field(
        c,
        "신인상: 이번 기간에 처음 나타난 에이전트의 작업 시간",
        format!("SELECT x.agent_name, SUM(x.duration_ms) / 3600000.0 AS v, {WHO} {base} AND (SELECT MIN(t3.start_ts) FROM agent_turns t3 WHERE t3.agent_name = x.agent_name) >= ?1 GROUP BY 1 ORDER BY v DESC LIMIT 8"),
    );
    push(&mut list, "신인상", "이번 기간에 처음 일한 에이전트 중 작업 시간 1위", f, hours);

    // thread-based awards (files)
    c.note(
        "검토·제안 상: 스레드 파일(thread.json + reply.md 첫 줄 판정)",
        "-- 파일 기반: ~/.local/share/adash/threads/*/thread.json, created_at ∈ [?1, ?2), from != 'human'; 판정 = reply.md 첫 줄에 '수정 필요' / '수락'",
    );
    let tsrc = c.src(None);
    let mut ts: Vec<crate::threads::Thread> = crate::threads::load_all()
        .into_iter()
        .filter(|t| t.from != "human" && t.created_at >= c.from && t.created_at < c.to && eligible.contains(&t.to))
        .collect();
    ts.sort_by_key(|t| t.created_at);
    let verdict = |t: &crate::threads::Thread| {
        t.reply_text()
            .and_then(|r| {
                r.lines().find(|l| !l.trim().is_empty()).map(|l| {
                    if l.contains("수정 필요") {
                        "fix"
                    } else if l.contains("수락") {
                        "ok"
                    } else {
                        ""
                    }
                    .to_string()
                })
            })
            .unwrap_or_default()
    };
    let kind =
        |t: &crate::threads::Thread| if t.magi_kind.as_deref() == Some("propose") || t.mode.as_deref() == Some("propose") { "propose" } else { "review" };
    // follow-up: the same requester asked the same agent again within 3 days
    let followed = |t: &crate::threads::Thread| {
        ts.iter().any(|u| u.from == t.from && u.to == t.to && u.created_at > t.created_at && u.created_at - t.created_at < 3 * 86_400_000)
    };
    let mut prop: HashMap<String, (f64, f64)> = HashMap::new(); // answered, followed
    let mut rev: HashMap<String, (f64, f64, f64)> = HashMap::new(); // answered, fix, fix→ok chains
    for t in ts.iter().filter(|t| t.reply_text().is_some()) {
        if kind(t) == "propose" {
            let e = prop.entry(t.to.clone()).or_default();
            e.0 += 1.0;
            if followed(t) {
                e.1 += 1.0;
            }
        } else {
            let e = rev.entry(t.to.clone()).or_default();
            e.0 += 1.0;
            if verdict(t) == "fix" {
                e.1 += 1.0;
                if ts.iter().any(|u| {
                    u.from == t.from && u.to == t.to && u.created_at > t.created_at && u.created_at - t.created_at < 3 * 86_400_000 && verdict(u) == "ok"
                }) {
                    e.2 += 1.0;
                }
            }
        }
    }
    let who = |n: &str| -> (String, String) {
        c.db.conn
            .query_row("SELECT agent_type, model FROM agent_turns WHERE agent_name = ?1 ORDER BY start_ts DESC LIMIT 1", [n], |r| {
                Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            })
            .unwrap_or_default()
    };
    let ranked = |m: Vec<(String, f64)>| -> Vec<(String, f64, String, String)> {
        let mut v: Vec<(String, f64, String, String)> = m
            .into_iter()
            .map(|(n, x)| {
                let (h, md) = who(&n);
                (n, x, h, md)
            })
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v.truncate(8);
        v
    };
    let p = ranked(prop.iter().map(|(n, (a, f))| (n.clone(), a * (0.5 + 0.5 * f / a))).collect());
    push(
        &mut list,
        "우수 제안자상",
        "점수 = 답한 제안 수 × (0.5 + 0.5 × 후속 요청률); 후속 요청률 = 답 뒤 3일 안에 같은 요청자가 다시 맡긴 비율(채택의 근사)",
        (p, tsrc.clone()),
        score,
    );
    let r = ranked(rev.iter().map(|(n, (a, _, chain))| (n.clone(), a + 2.0 * chain)).collect());
    push(&mut list, "우수 검토자상", "점수 = 검토 답 수 + 2 × ('수정 필요' 뒤 3일 안에 같은 쌍의 재요청이 '수락'으로 끝난 횟수)", (r, tsrc.clone()), score);
    let st = ranked(rev.iter().filter(|(_, v)| v.1 > 0.0 || v.0 >= 2.0).filter(|(_, v)| v.0 >= 2.0).map(|(n, (a, f, _))| (n.clone(), f / a * 100.0)).collect());
    push(&mut list, "엄격 심사상", "검토 답 중 첫 줄이 '수정 필요'인 비율(답 2건 이상)", (st, tsrc.clone()), pct_s);
    // 해결사상: pair whose first request reached an accepted reply fastest (minutes)
    let mut solve: HashMap<String, f64> = HashMap::new();
    for t in &ts {
        if let Some(ok) = ts
            .iter()
            .filter(|u| u.from == t.from && u.to == t.to && u.created_at >= t.created_at && u.created_at - t.created_at < 3 * 86_400_000 && verdict(u) == "ok")
            .filter_map(|u| u.events.iter().find(|e| e.kind == "answered").map(|e| e.at))
            .min()
        {
            let m = (ok - t.created_at) as f64 / 60_000.0;
            let key = format!("{} → {}", t.from, t.to);
            let e = solve.entry(key).or_insert(f64::MAX);
            *e = e.min(m);
        }
    }
    let mut sv: Vec<(String, f64, String, String)> = solve.into_iter().map(|(k, m)| (k, m, String::new(), String::new())).collect();
    sv.sort_by(|a, b| a.1.total_cmp(&b.1));
    push(&mut list, "해결사상", "검토 요청부터 '수락' 답까지 가장 짧았던 쌍(재요청 포함, 3일 안)", (sv, tsrc.clone()), mins);

    // one agent does not take both 최다 근무 and 신인: the rookie award passes to the next
    let top_worker = list.iter().find(|a| a.title == "최다 근무상").and_then(|a| a.field.first().map(|w| w.0.clone()));
    if let (Some(w), Some(r)) = (top_worker, list.iter_mut().find(|a| a.title == "신인상")) {
        if r.field.first().is_some_and(|x| x.0 == w) {
            r.field.remove(0);
            r.formula.push_str(" (최다 근무상 수상자는 제외)");
        }
    }
    list.retain(|a| !a.field.is_empty());
    let mut h = String::from("<section class=awards><h2>에이전트 시상식</h2>");
    if list.is_empty() {
        h.push_str("<p class=muted>이 기간에는 시상할 기록이 없습니다.</p></section>");
        return h;
    }
    // summary strip
    h.push_str("<div class=strip>");
    for a in &list {
        let _ = write!(h, "<span><em>{}</em> {}</span>", a.title, esc(&a.field[0].0));
    }
    h.push_str("</div>");
    if let Some(top) = list.iter().find(|a| a.title == "최다 근무상") {
        let _ = write!(h, "<div class=podwrap><div class=podt>최다 근무 시상대</div>{}</div>", podium(top));
    }
    h.push_str("<div class=cards2>");
    for a in &list {
        h.push_str(&card(a));
    }
    h.push_str("</div>");
    // 이름 짓기상: names agents gave themselves (registry, set in the period, else all)
    let reg = crate::registry::Registry::load();
    let mut named: Vec<(&String, &crate::registry::SelfName)> = reg.self_names.iter().filter(|(_, n)| n.set_at >= c.from && n.set_at < c.to).collect();
    named.sort_by_key(|x| x.1.set_at);
    c.note("이름 짓기상: 스스로 지은 이름(adash iam, 레지스트리 self_names.set_at ∈ [?1, ?2))", "-- 파일 기반: ~/.local/share/adash/registry.json self_names");
    let s = c.src(None);
    if !named.is_empty() {
        let _ = write!(
            h,
            "<div class=namers><b>이름 짓기상</b> 스스로 이름을 지은 에이전트: {}{s}</div>",
            named
                .iter()
                .map(|(_, n)| format!("<span class=nm title=\"{}\">{}</span>", esc(n.role.as_deref().unwrap_or("")), esc(&n.name)))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    h.push_str("</section>");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_skip_stopwords_and_prefer_repeated_phrases() {
        let texts = ["좋네 진행하자", "그럼 좋네 진행하자", "진행하자 해줘", "좋네 진행하자 지금", "the plan is fine", "해줘"];
        let top = top_expressions(&texts, 3);
        assert_eq!(top[0], ("좋네 진행하자".to_string(), 3));
        // stopwords never start or end an expression; singles fill the rest
        assert!(top.iter().all(|(e, _)| !e.starts_with("그럼 ") && !e.ends_with(" 지금")));
        assert!(top.iter().any(|(e, n)| e == "해줘" && *n == 2));
        assert_eq!(words("`a/b.md` 를 고쳐줘! 2026-10-30"), vec!["를", "고쳐줘"]);
        let comment = "File: reviews/r2.md\nSource: markdown\n\nLine 13\nExcerpt:\n> II B 쪽은 단정한다.\nUser comment: \"이 부분이 AI가 쓴 것 같은데.\"";
        assert_eq!(own_words(comment), "이 부분이 AI가 쓴 것 같은데.");
        assert_eq!(own_words("그림 다시\n```\ncode\n```\n좋네"), "그림 다시\n좋네");
        assert_eq!(own_words("봐 줘 <attachment name=\"a.md\">긴 내용</attachment> 고마워"), "봐 줘  고마워");
    }

    #[test]
    fn streak_and_focus_span() {
        let d = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        let days: BTreeSet<NaiveDate> = [d(10, 1), d(10, 2), d(10, 3), d(10, 5), d(10, 31), d(11, 1)].into_iter().collect();
        assert_eq!(longest_streak(&days), 3);
        assert_eq!(longest_streak(&BTreeSet::new()), 0);
        let m = 60_000;
        // 0,5,10 min chain (10 min) then a 30 min gap, then 50,60,75,90 (40 min)
        let ts = [0, 5 * m, 10 * m, 40 * m, 50 * m, 60 * m, 75 * m, 90 * m];
        assert_eq!(focus_span(&ts, 20 * m), Some((40 * m, 90 * m, 5)));
        assert_eq!(focus_span(&[0], 20 * m), None);
        assert_eq!(concurrency(&[(0, 10), (5, 20), (6, 8)]).0, 3);
    }
}
