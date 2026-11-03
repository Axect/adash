//! Change an agent's model or reasoning effort from the dashboard, the way each harness allows
//! it in its own terminal (checked on throwaway sessions, 2026-11-03):
//!
//! | harness | model | effort | scope |
//! |---|---|---|---|
//! | omp 18.15 | no: `/model` opens a picker whose Enter assigns a *role* (default, smol, …) | `/effort <level>` → `thinking_level_change` | this session |
//! | Claude Code 2.1.316 | `/model <alias>` | `/effort <level>` | also saved as the default for new sessions |
//! | Codex 0.164 | `/model <slug> --reasoning=<level>` | same command | this session ("for this session only") |
//! | Antigravity | no | no | — |
//!
//! Every change is confirmed from the session record (omp, Claude Code) or the harness's own
//! confirmation line on screen (Codex); otherwise the request fails.
use crate::collect::Agent;
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// A model the harness offers: (id to send, label, efforts).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelOpt {
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
}

const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Claude Code's model aliases (`claude --help`: "an alias for the latest model").
const CLAUDE_MODELS: &[(&str, &str)] = &[("opus", "Opus (latest)"), ("sonnet", "Sonnet (latest)"), ("haiku", "Haiku (latest)"), ("fable", "Fable (latest)")];

/// `omp models --json` → models with their thinking levels.
pub fn parse_omp_models(v: &Value) -> Vec<ModelOpt> {
    let list = if v.is_array() { v } else { &v["models"] };
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|x| x["kind"].as_str().is_none_or(|k| k == "chat"))
        .filter_map(|x| {
            let id = x["selector"].as_str()?.to_string();
            let efforts = x["thinking"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
            Some(ModelOpt { label: x["name"].as_str().unwrap_or(&id).to_string(), id, efforts })
        })
        .collect()
}

/// `codex debug models` → listed models with their reasoning levels.
pub fn parse_codex_models(v: &Value) -> Vec<ModelOpt> {
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| x["visibility"].as_str().is_none_or(|s| s == "list"))
        .filter_map(|x| {
            let id = x["slug"].as_str()?.to_string();
            let efforts = x["supported_reasoning_levels"]
                .as_array()
                .map(|a| a.iter().filter_map(|e| e["effort"].as_str().map(String::from)).collect())
                .unwrap_or_default();
            Some(ModelOpt { label: x["display_name"].as_str().unwrap_or(&id).to_string(), id, efforts })
        })
        .collect()
}

/// The harness's model list, cached an hour (it runs the harness binary).
fn models_of(kind: &str) -> Vec<ModelOpt> {
    static CACHE: LazyLock<Mutex<std::collections::HashMap<String, (i64, Vec<ModelOpt>)>>> = LazyLock::new(Default::default);
    if let Some((at, v)) = CACHE.plock().get(kind) {
        if now_ms() - at < 3_600_000 && !v.is_empty() {
            return v.clone();
        }
    }
    let run = |bin: &str, args: &[&str]| -> Option<Value> {
        let b = crate::launch::find_bin(bin)?;
        let o = crate::platform::timeout(20).arg(b).args(args).output().ok()?;
        serde_json::from_slice(&o.stdout).ok()
    };
    let v = match kind {
        "omp" => run("omp", &["models", "--json"]).map(|v| parse_omp_models(&v)).unwrap_or_default(),
        "codex" => run("codex", &["debug", "models"]).map(|v| parse_codex_models(&v)).unwrap_or_default(),
        "claude" => CLAUDE_MODELS
            .iter()
            .map(|(id, l)| ModelOpt { id: id.to_string(), label: l.to_string(), efforts: CLAUDE_EFFORTS.iter().map(|s| s.to_string()).collect() })
            .collect(),
        _ => vec![],
    };
    if !v.is_empty() {
        CACHE.plock().insert(kind.to_string(), (now_ms(), v.clone()));
    }
    v
}

fn caps(kind: &str) -> (bool, bool, bool, &'static str) {
    match kind {
        "omp" => (false, true, false, "omp changes the model only through its /model picker, whose selection assigns a role (default, smol, …) in your omp settings; change it in the terminal. Effort: /effort, this session only."),
        "claude" => (true, true, true, "/model and /effort; Claude Code also saves the choice as the default for new sessions."),
        "codex" => (true, true, false, "/model <model> --reasoning=<effort>, this session only."),
        _ => (false, false, false, "This harness has no command to change the model or effort of a running session; change it in the terminal."),
    }
}

/// `GET /api/agents/{name}/models`.
pub fn view(a: &Agent) -> Value {
    let (can_model, can_effort, persists, how) = caps(&a.agent_type);
    let u = crate::usage::view(a);
    let list = models_of(&a.agent_type);
    let models: Vec<Value> = list.iter().map(|m| json!({"id": m.id, "label": m.label, "efforts": m.efforts})).collect();
    // efforts of the current model (Claude aliases and unknown models: every level the harness has)
    let cur = u["model"].as_str().unwrap_or("");
    let efforts: Vec<String> = if !can_effort {
        vec![]
    } else {
        list.iter().find(|m| m.id == cur || cur.ends_with(&format!("/{}", m.id)) || cur.contains(&m.id)).map(|m| m.efforts.clone()).unwrap_or_else(|| {
            let mut all: Vec<String> = list.iter().flat_map(|m| m.efforts.clone()).collect();
            all.dedup();
            let mut seen = std::collections::HashSet::new();
            all.retain(|e| seen.insert(e.clone()));
            all
        })
    };
    json!({
        "harness": a.agent_type,
        "current": {"model": u["model"], "effort": u["thinking"]},
        "models": models, "efforts": efforts, "canSetModel": can_model, "canSetEffort": can_effort,
        "persistsDefault": persists, "how": how,
    })
}

/// The keystrokes (one command line) for a change, after validating against the model list.
pub fn command(kind: &str, current_model: Option<&str>, model: Option<&str>, effort: Option<&str>, models: &[ModelOpt]) -> Result<Vec<String>, (u16, String)> {
    let (can_model, can_effort, _, how) = caps(kind);
    let bad = |m: String| Err((400, m));
    if model.is_none() && effort.is_none() {
        return bad("model or effort required".into());
    }
    if model.is_some() && !can_model {
        return Err((409, how.to_string()));
    }
    if effort.is_some() && !can_effort {
        return Err((409, how.to_string()));
    }
    let find = |id: &str| models.iter().find(|m| m.id == id);
    if let Some(m) = model {
        if find(m).is_none() {
            return bad(format!("unknown model {m}"));
        }
    }
    if let Some(e) = effort {
        // the effort must be one the (new or current) model offers; Claude aliases list them all
        let of = model.or(current_model).and_then(|m| find(m).or_else(|| models.iter().find(|x| m.ends_with(&x.id))));
        let ok = match of {
            Some(m) => m.efforts.iter().any(|x| x == e),
            None => models.iter().any(|m| m.efforts.iter().any(|x| x == e)),
        };
        if !ok {
            return bad(format!("effort {e} is not offered for this model"));
        }
    }
    Ok(match kind {
        "omp" => vec![format!("/effort {}", effort.unwrap_or_default())],
        "claude" => {
            let mut v = vec![];
            if let Some(m) = model {
                v.push(format!("/model {m}"));
            }
            if let Some(e) = effort {
                v.push(format!("/effort {e}"));
            }
            v
        }
        "codex" => {
            let m = model.or(current_model).ok_or((409, "current model unknown; choose a model too".to_string()))?;
            vec![match effort {
                Some(e) => format!("/model {m} --reasoning={e}"),
                None => format!("/model {m}"),
            }]
        }
        _ => return Err((409, how.to_string())),
    })
}

/// Lines of a session file added after byte `from`.
fn new_lines(path: &std::path::Path, from: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        if f.seek(SeekFrom::Start(from)).is_ok() {
            let _ = f.read_to_string(&mut s);
        }
    }
    s
}

/// Did the harness confirm the change? omp: a `thinking_level_change` to the level; Claude
/// Code: its "Set model to …" / "Set effort level to <level>" command output; Codex: "Model
/// changed to <model> <effort>" on screen.
pub fn confirmed(kind: &str, has_file: bool, appended: &str, screen: &str, model: Option<&str>, effort: Option<&str>) -> bool {
    match kind {
        // a session not written yet (no message so far) has no record: omp's own reply line
        "omp" => effort.is_some_and(|e| {
            appended.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).any(|v| v["type"] == "thinking_level_change" && v["thinkingLevel"] == e)
                || (!has_file && screen.lines().any(|l| l.trim() == format!("Thinking set to {e}.")))
        }),
        "claude" => {
            let m_ok = model.is_none_or(|_| appended.contains("Set model to"));
            let e_ok = effort.is_none_or(|e| appended.contains(&format!("Set effort level to {e}")));
            m_ok && e_ok
        }
        "codex" => screen
            .lines()
            .any(|l| l.contains("Model changed to") && model.is_none_or(|m| l.contains(m)) && effort.is_none_or(|e| l.split_whitespace().any(|w| w == e))),
        _ => false,
    }
}

/// `POST /api/agents/{name}/model {model?, effort?}`: type the command line(s) and confirm.
pub fn apply(a: &Agent, model: Option<&str>, effort: Option<&str>) -> Result<Value, (u16, String)> {
    if matches!(a.state.as_str(), "working" | "asking") {
        return Err((409, format!("{} is {}; change the model when it is waiting", a.name, a.state)));
    }
    if a.draft.as_ref().and_then(|d| d["text"].as_str()).is_some_and(|t| !t.trim().is_empty()) {
        return Err((409, "the input box holds unsent text; send or clear it first".into()));
    }
    let h = a.handle.clone().ok_or((409, "agent has no terminal".to_string()))?;
    let cur = crate::usage::view(a);
    let lines = command(&a.agent_type, cur["model"].as_str(), model, effort, &models_of(&a.agent_type))?;
    let path = a.session_path.clone();
    let start = path.as_deref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
    let lk = crate::term::send_lock(&h);
    let _g = lk.plock();
    for l in &lines {
        crate::term::send_line(&h, l).map_err(|e| (502, e))?;
        std::thread::sleep(Duration::from_millis(600));
    }
    for _ in 0..16 {
        std::thread::sleep(Duration::from_millis(500));
        let appended = path.as_deref().map(|p| new_lines(p, start)).unwrap_or_default();
        let screen = crate::term::screen(&h).map(|v| v.join("\n")).unwrap_or_default();
        if confirmed(&a.agent_type, path.is_some(), &appended, &screen, model, effort) {
            return Ok(json!({"ok": true, "name": a.name, "model": model, "effort": effort, "sent": lines}));
        }
    }
    crate::term::snapshot(&h, "model-change-unconfirmed");
    Err((502, format!("sent {lines:?} but {} did not confirm the change; check the terminal", a.agent_type)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Vec<ModelOpt> {
        vec![
            ModelOpt { id: "gpt-6-luna".into(), label: "Luna".into(), efforts: vec!["low".into(), "high".into()] },
            ModelOpt { id: "gpt-6-sol".into(), label: "Sol".into(), efforts: vec!["medium".into(), "xhigh".into()] },
        ]
    }

    #[test]
    fn commands_per_harness() {
        assert_eq!(command("codex", Some("gpt-6-sol"), Some("gpt-6-luna"), Some("high"), &opts()).unwrap(), vec!["/model gpt-6-luna --reasoning=high"]);
        assert_eq!(command("codex", Some("gpt-6-sol"), None, Some("xhigh"), &opts()).unwrap(), vec!["/model gpt-6-sol --reasoning=xhigh"]);
        assert_eq!(command("codex", Some("gpt-6-sol"), None, Some("low"), &opts()).unwrap_err().0, 400, "luna's effort on sol");
        assert_eq!(command("codex", None, Some("gpt-7"), None, &opts()).unwrap_err().0, 400, "unknown model");
        let omp = vec![ModelOpt { id: "anthropic/claude-haiku-4-5".into(), label: "H".into(), efforts: vec!["low".into(), "medium".into()] }];
        assert_eq!(command("omp", Some("anthropic/claude-haiku-4-5"), None, Some("low"), &omp).unwrap(), vec!["/effort low"]);
        assert_eq!(command("omp", None, Some("anthropic/claude-haiku-4-5"), None, &omp).unwrap_err().0, 409, "omp model: terminal only");
        assert_eq!(command("antigravity", None, None, Some("low"), &[]).unwrap_err().0, 409);
        let cl = models_of("claude");
        assert_eq!(command("claude", Some("claude-opus-5-5"), Some("sonnet"), Some("high"), &cl).unwrap(), vec!["/model sonnet", "/effort high"]);
        // no shell or key injection through the ids: only listed values pass
        assert_eq!(command("claude", None, Some("opus\n/exit"), None, &cl).unwrap_err().0, 400);
    }

    #[test]
    fn confirmations_come_from_the_harness() {
        let rec = r#"{"type":"thinking_level_change","thinkingLevel":"low","configured":"low"}"#;
        assert!(confirmed("omp", true, rec, "", None, Some("low")));
        assert!(!confirmed("omp", true, rec, "", None, Some("high")));
        assert!(!confirmed("omp", true, "", "Thinking set to low.", None, Some("low")), "with a session file, the record decides");
        assert!(confirmed("omp", false, "", " Thinking set to low.", None, Some("low")), "no file yet: the reply line");
        let cl = "<local-command-stdout>Set model to `Haiku 5.5` and saved as your default</local-command-stdout>\n<local-command-stdout>Set effort level to low (saved as your default for new sessions)";
        assert!(confirmed("claude", true, cl, "", Some("haiku"), Some("low")));
        assert!(!confirmed("claude", true, cl, "", Some("haiku"), Some("high")));
        let sc = "• Model changed to gpt-6-luna high for this session only";
        assert!(confirmed("codex", false, "", sc, Some("gpt-6-luna"), Some("high")));
        assert!(!confirmed("codex", false, "", sc, Some("gpt-6-sol"), None));
    }

    #[test]
    fn model_lists_parse() {
        let o = parse_omp_models(
            &serde_json::json!([{"selector":"anthropic/claude-fable-5","name":"Claude Fable 5","kind":"chat","thinking":["low","high"]},{"selector":"x/embed","kind":"embedding"}]),
        );
        assert_eq!(o, vec![ModelOpt { id: "anthropic/claude-fable-5".into(), label: "Claude Fable 5".into(), efforts: vec!["low".into(), "high".into()] }]);
        let c = parse_codex_models(
            &serde_json::json!({"models":[{"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list","supported_reasoning_levels":[{"effort":"low"},{"effort":"max"}]},{"slug":"hidden","visibility":"hide"}]}),
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].efforts, vec!["low", "max"]);
    }
}
