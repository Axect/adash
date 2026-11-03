//! "정하실 것" extraction from an agent's latest answer. Strict: only the numbered/bulleted list
//! under a decisions heading (`## 정하실 것`, `## 결정 필요`, `## Decisions`, any heading level,
//! or a bold-only line `**정하실 것**`) up to the next heading. Options only from the agents'
//! writing rule: `label: A / B / C`. No guessing from prose (2026-10-30 user decision; the rule
//! is in each harness's global instructions).
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::LazyLock;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Decision {
    pub id: String,
    pub text: String,
    pub options: Vec<String>,
}

/// Accepted section titles (normalized: lowercase, no `#`/`*`, no trailing colon).
const HEADINGS: &[&str] = &["정하실 것", "정해 주실 것", "정해 주셔야 할 것", "결정 필요", "결정이 필요한 것", "decisions", "decisions needed"];

static LIST_ITEM: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\s*(?:\d+[.)]|[-*•])\s+(.*)$").unwrap());

/// A heading line: markdown `#…` or a line that is only bold text.
fn heading_text(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = if t.starts_with('#') {
        t.trim_start_matches('#').trim()
    } else if t.starts_with("**") && t.ends_with("**") && t.len() > 4 {
        &t[2..t.len() - 2]
    } else {
        return None;
    };
    Some(inner.trim().trim_matches('*').trim().trim_end_matches([':', '：']).trim().to_lowercase())
}

/// Split `s` at `sep` only outside brackets ()[]{}（）「」『』, quotes ("" “” ‘’), backticks and
/// `$…$` math. None when the brackets or quotes do not close (the caller shows no options).
/// ASCII `'` is not a quote: apostrophes ("user's") are common in prose.
fn split_top(s: &str, sep: &str) -> Option<Vec<String>> {
    let mut stack: Vec<char> = vec![];
    let (mut tick, mut math, mut dq) = (false, false, false);
    let mut parts = vec![];
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if tick {
            tick = c != '`';
            continue;
        }
        if math {
            math = c != '$';
            continue;
        }
        if dq {
            dq = c != '"';
            continue;
        }
        match c {
            '`' => tick = true,
            '$' => math = true,
            '"' => dq = true,
            '(' | '[' | '{' | '（' | '「' | '『' | '“' | '‘' => stack.push(c),
            ')' | ']' | '}' | '）' | '」' | '』' | '”' | '’' => {
                let want = match c {
                    ')' => '(',
                    ']' => '[',
                    '}' => '{',
                    '）' => '（',
                    '」' => '「',
                    '』' => '『',
                    '”' => '“',
                    _ => '‘',
                };
                if stack.pop() != Some(want) {
                    return None;
                }
            }
            _ if stack.is_empty() && s[i..].starts_with(sep) => {
                parts.push(s[start..i].to_string());
                start = i + sep.len();
                for _ in 1..sep.chars().count() {
                    it.next();
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() || tick || math || dq {
        return None;
    }
    parts.push(s[start..].to_string());
    Some(parts)
}

/// `label: A / B / C` → (label, [A, B, C]); anything else → (text, []). Only top-level " / "
/// separates options.
pub fn options_of(item: &str) -> (String, Vec<String>) {
    let split = item.find(": ").map(|i| (i, 2)).or_else(|| item.find('：').map(|i| (i, '：'.len_utf8())));
    if let Some((i, n)) = split {
        let opts: Vec<String> = split_top(&item[i + n..], " / ")
            .unwrap_or_default()
            .into_iter()
            .map(|o| o.trim().trim_end_matches(['.', '。']).trim().to_string())
            .filter(|o| !o.is_empty())
            .collect();
        if opts.len() >= 2 {
            return (item[..i].trim().to_string(), opts);
        }
    }
    (item.trim().to_string(), vec![])
}

pub fn extract(msg: &str) -> Vec<Decision> {
    let lines: Vec<&str> = msg.lines().collect();
    let mut items: Vec<String> = vec![];
    let mut i = 0;
    while i < lines.len() {
        if heading_text(lines[i]).is_some_and(|h| HEADINGS.contains(&h.as_str())) {
            i += 1;
            while i < lines.len() && heading_text(lines[i]).is_none() {
                if let Some(c) = LIST_ITEM.captures(lines[i]) {
                    // only top-level items; an indented line continues the previous item
                    if lines[i].starts_with(' ') && !items.is_empty() && !lines[i].trim_start().chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        let last = items.last_mut().unwrap();
                        last.push(' ');
                        last.push_str(c[1].trim());
                    } else {
                        items.push(c[1].trim().to_string());
                    }
                } else if lines[i].starts_with(' ') && !lines[i].trim().is_empty() {
                    // an indented line under an item continues it
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(lines[i].trim());
                    }
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    items
        .into_iter()
        .filter(|t| !t.is_empty())
        .enumerate()
        .map(|(k, t)| {
            let (text, options) = options_of(&t);
            Decision { id: format!("d{}", k + 1), text, options }
        })
        .collect()
}

/// Agent fields: (`decisions` JSON, `decisionsFrom` = message hash).
pub fn for_message(msg: &str) -> (Value, Option<String>) {
    let d = extract(msg);
    if d.is_empty() || msg.trim().is_empty() {
        return (json!([]), None);
    }
    use sha2::Digest;
    let id = format!("{:x}", sha2::Sha256::digest(msg.as_bytes()));
    (json!(d), Some(id[..12].to_string()))
}

/// Hide decisions the human already answered: any human input (message, ask answer, dashboard
/// send — all in `last_human_at`), a btw question (`btw_at`) or an acknowledgement after the
/// answer that carried them. A newer answer with new decisions shows them again.
pub fn settle(a: &mut crate::collect::Agent, btw_at: Option<i64>) {
    let Some(said) = a.last_answer_at else { return };
    let replied = [a.last_human_at, a.acked_at, btw_at].into_iter().flatten().max();
    if replied.is_some_and(|r| r > said) {
        a.decisions = Value::Array(vec![]);
        a.decisions_answered = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_decisions_section_counts() {
        // the section under "## 정해 주셔야 할 것" stays as written
        let d = extract(include_str!("testdata/decisions_review_reply.txt"));
        assert_eq!(d.len(), 4);
        assert_eq!(d[0].text, "남쪽 필드 보정을 토요일에 돌릴지, 월요일에 돌릴지");
        assert!(d.iter().all(|x| x.options.is_empty())); // no `label: A / B` form there
                                                         // false positives: plain prose, and a review report that mentions "결정" in its body
        assert!(extract("파일 이름을 되돌렸습니다. 1쪽은 편지, 2쪽은 추천인 목록입니다.").is_empty());
        assert!(extract(include_str!("testdata/decisions_peer_report.txt")).is_empty());
        // trailing question lists without the heading are not decisions any more
        assert!(extract("검토를 마쳤습니다.\n\n1. 그림 3을 본문에 둘까요, 부록으로 옮길까요?\n2. A안/B안 중 어느 표기를 쓸지\n").is_empty());
    }

    #[test]
    fn rule_form_options_and_section_bounds() {
        let msg = "고쳤습니다.\n\n**정하실 것**\n1. 메일 발송일: 10-31(토) / 11-02(월)\n2. 보고서 판: 수치만 / 새 판\n3. 외부 검토자를 위원으로 넣을지\n   (규정 확인 못 함)\n\n## 다음 할 일\n- 업로드 체크리스트 정리\n";
        let d = extract(msg);
        assert_eq!(d.len(), 3);
        assert_eq!((d[0].text.as_str(), d[0].options.clone()), ("메일 발송일", vec!["10-31(토)".to_string(), "11-02(월)".to_string()]));
        assert_eq!(d[1].options, vec!["수치만", "새 판"]);
        assert_eq!((d[2].text.as_str(), d[2].options.len()), ("외부 검토자를 위원으로 넣을지 (규정 확인 못 함)", 0));
        let en = extract("Done.\n\n### Decisions\n- Send date: Oct 31 / Nov 2\n");
        assert_eq!(en[0].options, vec!["Oct 31", "Nov 2"]);
        // a colon without " / " alternatives is just text
        assert_eq!(options_of("범위: 수치 갱신만 할지"), ("범위: 수치 갱신만 할지".to_string(), vec![]));
    }

    #[test]
    fn options_split_only_at_top_level() {
        let o = |s: &str| super::options_of(s).1;
        assert_eq!(o("문단 구성: 두 문단(1–5 / 6–8) / 한 문단 유지"), ["두 문단(1–5 / 6–8)", "한 문단 유지"]);
        assert_eq!(o("식: $a / b$ 로 쓰기 / $a b^{-1}$ 로 쓰기"), ["$a / b$ 로 쓰기", "$a b^{-1}$ 로 쓰기"]);
        assert_eq!(o("경로: `src/a / b.rs` / `src/c.rs`"), ["`src/a / b.rs`", "`src/c.rs`"]);
        assert_eq!(o("중첩: A(x [1 / 2] / y) / B"), ["A(x [1 / 2] / y)", "B"]);
        assert_eq!(o("인용: “a / b” / 「c / d」 / e"), ["“a / b”", "「c / d」", "e"]);
        assert!(o("짝 없음: A(1 / 2 / B").is_empty(), "unbalanced brackets: no options");
        assert_eq!(o("영어: the user's draft / keep it"), ["the user's draft", "keep it"]);
    }

    #[test]
    fn answered_decisions_disappear_and_new_ones_return() {
        let msg = "작업 계획입니다.\n\n## 정하실 것\n1. 메일 발송일: 10-31(토) / 11-02(월)\n2. 공저자 범위\n";
        let fresh = |answer: i64| {
            let mut a = crate::collect::Agent { last_answer_at: Some(answer), ..Default::default() };
            (a.decisions, a.decisions_from) = for_message(msg);
            a
        };
        let count = |a: &crate::collect::Agent| a.decisions.as_array().map_or(0, |d| d.len());
        let mut a = fresh(1_000);
        settle(&mut a, None);
        assert_eq!((count(&a), a.decisions_answered), (2, false), "still open");
        a.last_human_at = Some(2_000); // the user replied; the agent is working on it
        settle(&mut a, None);
        assert_eq!((count(&a), a.decisions_answered), (0, true));
        let mut b = fresh(1_000);
        b.acked_at = Some(1_500); // 'e' (seen) folds them too
        settle(&mut b, None);
        assert_eq!(count(&b), 0);
        let mut c = fresh(1_000);
        settle(&mut c, Some(1_200)); // a btw question
        assert_eq!(count(&c), 0);
        let mut d = fresh(3_000); // a newer answer with new decisions after the reply
        d.last_human_at = Some(2_000);
        settle(&mut d, None);
        assert_eq!((count(&d), d.decisions_answered), (2, false));
    }
}
