//! Web Push (PWA) for phones: VAPID keys in `~/.local/share/adash/vapid/` (0600), per-device
//! subscriptions with event filters, quiet hours and "only when the desktop is idle".
use crate::util::{atomic_write, now_ms, Res};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

/// Push services browsers use (Chrome/Edge FCM, Firefox autopush, Apple, Windows WNS).
const PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "android.googleapis.com",
    "updates.push.services.mozilla.com",
    "push.services.mozilla.com",
    "web.push.apple.com",
    "notify.windows.com",
];

fn push_host_ok(host: &str) -> bool {
    PUSH_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

#[cfg(test)]
mod host_tests {
    #[test]
    fn only_browser_push_services() {
        assert!(super::push_host_ok("fcm.googleapis.com"));
        assert!(super::push_host_ok("wns2-by3p.notify.windows.com"));
        assert!(!super::push_host_ok("127.0.0.1"));
        assert!(!super::push_host_ok("evilfcm.googleapis.com.attacker.net"));
        assert!(!super::push_host_ok("notnotify.windows.com"));
    }
}

pub const EVENTS: &[&str] = &["asking", "error", "network", "human_answer", "relay_held", "escalated", "waiting_after_human", "report"];

fn dir() -> PathBuf {
    crate::platform::data_dir().join("vapid")
}

fn subs_path() -> PathBuf {
    crate::platform::data_dir().join("push.json")
}

fn chmod600(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

fn b64url(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..(c.len() + 1) {
            out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// VAPID key pair (P-256) generated once with openssl; returns the base64url public key
/// (uncompressed point) the browser needs as `applicationServerKey`.
pub fn public_key() -> Res<String> {
    let d = dir();
    let key = d.join("private.pem");
    if !key.exists() {
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        let st = Command::new("openssl")
            .args(["ecparam", "-genkey", "-name", "prime256v1", "-noout", "-out"])
            .arg(&key)
            .status()
            .map_err(|e| format!("openssl: {e}"))?;
        if !st.success() {
            return Err("openssl ecparam failed".into());
        }
        chmod600(&key);
    }
    let out = Command::new("openssl").args(["ec", "-pubout", "-outform", "DER", "-in"]).arg(&key).output().map_err(|e| format!("openssl: {e}"))?;
    let der = out.stdout;
    if der.len() < 65 {
        return Err("could not read the VAPID public key".into());
    }
    Ok(b64url(&der[der.len() - 65..]))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "all_events")]
    pub events: Vec<String>,
    /// {"from": "23:00", "to": "07:30"}
    #[serde(default)]
    pub quiet_hours: Option<Value>,
    #[serde(default)]
    pub only_when_idle: bool,
}

fn all_events() -> Vec<String> {
    EVENTS.iter().map(|s| s.to_string()).collect()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sub {
    pub endpoint: String,
    pub subscription: Value,
    pub settings: Settings,
    pub device_id: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Subs {
    pub subs: Vec<Sub>,
}

impl Subs {
    pub fn load() -> Subs {
        std::fs::read_to_string(subs_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self) -> Res<()> {
        crate::util::guard_config(&subs_path(), |t| serde_json::from_str::<Subs>(t).is_ok())?;
        atomic_write(&subs_path(), serde_json::to_string_pretty(self).unwrap().as_bytes())?;
        chmod600(&subs_path());
        Ok(())
    }
    pub fn subscribe(&mut self, subscription: Value, settings: Option<Value>, device: Option<String>) -> Res<()> {
        let endpoint = subscription["endpoint"].as_str().filter(|e| e.starts_with("https://")).ok_or("subscription.endpoint (https) required")?.to_string();
        // only real browser push services (no arbitrary URL the server would POST to)
        let host = endpoint.trim_start_matches("https://").split(['/', ':']).next().unwrap_or("").to_ascii_lowercase();
        if !push_host_ok(&host) {
            return Err(format!("push endpoint host {host} is not a known push service"));
        }
        let settings: Settings =
            settings.and_then(|s| serde_json::from_value(s).ok()).unwrap_or(Settings { events: all_events(), quiet_hours: None, only_when_idle: false });
        self.subs.retain(|s| s.endpoint != endpoint);
        self.subs.push(Sub { endpoint, subscription, settings, device_id: device, created_at: now_ms() });
        self.save()
    }
    pub fn unsubscribe(&mut self, endpoint: &str) -> Res<bool> {
        let n = self.subs.len();
        self.subs.retain(|s| s.endpoint != endpoint);
        self.save()?;
        Ok(self.subs.len() != n)
    }
}

pub fn idle_supported() -> bool {
    Command::new("sh").args(["-c", "command -v xprintidle"]).output().is_ok_and(|o| o.status.success())
}

fn desktop_idle_ms() -> Option<u64> {
    let o = Command::new("xprintidle").output().ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// "HH:MM" window, possibly over midnight.
pub fn in_quiet_hours(q: &Value, now_min: u32) -> bool {
    let p = |s: &str| -> Option<u32> {
        let (h, m) = s.split_once(':')?;
        Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
    };
    let (Some(f), Some(t)) = (q["from"].as_str().and_then(p), q["to"].as_str().and_then(p)) else { return false };
    if f <= t {
        now_min >= f && now_min < t
    } else {
        now_min >= f || now_min < t
    }
}

/// Classify a desktop notification title into a push event.
pub fn event_of(title: &str) -> &'static str {
    if title.starts_with("오류 · ") {
        "error"
    } else if title.starts_with("네트워크 ") {
        "network"
    } else if title.starts_with("질문 대기") {
        "asking"
    } else if title.starts_with("adash 답변 도착") {
        "human_answer"
    } else if title.starts_with("검토 답변 보류") || title.starts_with("adash 전송 보류") {
        "relay_held"
    } else if title.contains("보고서가 나왔습니다") {
        "report"
    } else if title.contains("상한 도달") {
        "escalated"
    } else {
        "waiting_after_human"
    }
}

/// Send one event to every subscription that wants it. Gone endpoints (404/410) are dropped.
pub fn dispatch(event: &str, title: &str, body: &str, agent: Option<&str>) {
    let subs = Subs::load();
    if subs.subs.is_empty() {
        return;
    }
    let now_min = {
        use chrono::Timelike;
        let n = chrono::Local::now();
        n.hour() * 60 + n.minute()
    };
    let payload =
        json!({"title": title, "body": crate::util::truncate(body, 160), "agent": agent, "tag": format!("{event}:{}", agent.unwrap_or(""))}).to_string();
    let key = dir().join("private.pem");
    let mut gone = vec![];
    for s in &subs.subs {
        // subscriptions saved before "error" existed get errors along with questions
        let wants = |e: &str| s.settings.events.iter().any(|x| x == e);
        if !(wants(event) || (matches!(event, "error" | "network") && wants("asking"))) {
            continue;
        }
        if s.settings.quiet_hours.as_ref().is_some_and(|q| in_quiet_hours(q, now_min)) {
            continue;
        }
        if s.settings.only_when_idle && desktop_idle_ms().is_some_and(|ms| ms < 5 * 60_000) {
            continue;
        }
        match send_one(&s.subscription, &payload, &key) {
            Ok(()) => {}
            Err(e) if e.contains("EndpointNotValid") || e.contains("EndpointNotFound") || e.contains("410") || e.contains("404") => {
                gone.push(s.endpoint.clone())
            }
            Err(e) => eprintln!("adash: web push to {}…: {e}", s.endpoint.chars().take(40).collect::<String>()),
        }
    }
    if !gone.is_empty() {
        // re-read: a subscribe/revoke may have happened while we were sending
        let mut now = Subs::load();
        now.subs.retain(|s| !gone.contains(&s.endpoint));
        let _ = now.save();
    }
}

fn send_one(sub: &Value, payload: &str, key: &std::path::Path) -> Res<()> {
    use web_push::{ContentEncoding, IsahcWebPushClient, SubscriptionInfo, VapidSignatureBuilder, WebPushClient, WebPushMessageBuilder};
    let info =
        SubscriptionInfo::new(sub["endpoint"].as_str().unwrap_or(""), sub["keys"]["p256dh"].as_str().unwrap_or(""), sub["keys"]["auth"].as_str().unwrap_or(""));
    let pem = std::fs::File::open(key).map_err(|e| e.to_string())?;
    let mut sig = VapidSignatureBuilder::from_pem(pem, &info).map_err(|e| e.to_string())?;
    sig.add_claim("sub", "mailto:adash@localhost");
    let sig = sig.build().map_err(|e| e.to_string())?;
    let mut b = WebPushMessageBuilder::new(&info);
    b.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
    b.set_vapid_signature(sig);
    b.set_ttl(3600);
    let msg = b.build().map_err(|e| e.to_string())?;
    let client = IsahcWebPushClient::new().map_err(|e| e.to_string())?;
    futures_lite::future::block_on(client.send(msg)).map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_and_event_mapping() {
        let q = json!({"from": "23:00", "to": "07:30"});
        assert!(in_quiet_hours(&q, 23 * 60 + 5) && in_quiet_hours(&q, 3 * 60) && !in_quiet_hours(&q, 8 * 60));
        assert!(in_quiet_hours(&json!({"from": "12:00", "to": "13:00"}), 12 * 60 + 30));
        assert_eq!(event_of("질문 대기 · PLANNER · galaxy-survey"), "asking");
        assert_eq!(event_of("검토 답변 보류: CRITIC 입력창에 초안"), "relay_held");
        assert_eq!(event_of("adash 답변 도착: CRITIC"), "human_answer");
        assert_eq!(event_of("PLANNER · galaxy-survey"), "waiting_after_human");
        assert_eq!(event_of("주간 보고서가 나왔습니다"), "report");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }
}
