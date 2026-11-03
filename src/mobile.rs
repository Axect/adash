//! Phase C: phone access through `tailscale serve` (HTTPS, tailnet only).
//! The server still binds 127.0.0.1; requests proxied by Tailscale carry `Tailscale-User-Login`.
//! Rules: direct loopback requests (no Tailscale headers) are the local desktop and allowed;
//! proxied requests need an allowed tailnet login AND an approved device (cookie token) —
//! a new device gets a 6-digit code and waits until the user approves it on the desktop.
use crate::util::{atomic_write, now_ms, Res};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub login: String,
    pub approved: bool,
    #[serde(default)]
    pub revoked: bool,
    pub code: String,
    pub created_at: i64,
    pub last_seen: i64,
    /// secret kept in an HttpOnly cookie; never sent back in listings
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub token: String,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Devices {
    pub devices: Vec<Device>,
}

fn path() -> PathBuf {
    if cfg!(test) {
        // unit tests must never touch the real registry (a test run once replaced it)
        return std::env::temp_dir().join(format!("adash-test-devices-{}.json", std::process::id()));
    }
    crate::platform::data_dir().join("devices.json")
}

fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

impl Devices {
    pub fn load() -> Devices {
        std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Res<()> {
        let p = path();
        crate::util::guard_config(&p, |t| serde_json::from_str::<Devices>(t).is_ok())?;
        atomic_write(&p, serde_json::to_string_pretty(self).unwrap().as_bytes())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())
    }

    pub fn by_token(&mut self, token: &str) -> Option<&mut Device> {
        if token.len() < 32 {
            return None;
        }
        self.devices.iter_mut().find(|d| d.token == token)
    }

    /// New pending device for a proxied browser; returns (device, token for the cookie).
    pub fn enroll(&mut self, login: &str, user_agent: &str) -> Device {
        let ua = user_agent.to_lowercase();
        let name = if ua.contains("android") {
            "Android"
        } else if ua.contains("iphone") || ua.contains("ipad") {
            "iOS"
        } else if ua.contains("mac os") {
            "Mac"
        } else if ua.contains("windows") {
            "Windows"
        } else {
            "브라우저"
        };
        let browser = if ua.contains("firefox") {
            "Firefox"
        } else if ua.contains("edg/") {
            "Edge"
        } else if ua.contains("chrome") {
            "Chrome"
        } else if ua.contains("safari") {
            "Safari"
        } else {
            ""
        };
        let code: String = random_hex(4).chars().filter(|c| c.is_ascii_digit()).chain("000000".chars()).take(6).collect();
        let d = Device {
            id: format!("D{}", random_hex(4)),
            name: format!("{name} {browser}").trim().to_string(),
            login: login.into(),
            approved: false,
            revoked: false,
            code,
            created_at: now_ms(),
            last_seen: now_ms(),
            token: random_hex(24),
        };
        self.devices.push(d.clone());
        d
    }

    pub fn listing(&self) -> Value {
        let list: Vec<Value> = self
            .devices
            .iter()
            .map(|d| json!({"id": d.id, "name": d.name, "login": d.login, "approved": d.approved && !d.revoked, "revoked": d.revoked, "pending": !d.approved && !d.revoked, "lastSeen": d.last_seen, "createdAt": d.created_at, "code": d.code}))
            .collect();
        json!({"devices": list})
    }
}

/// Tailnet identity from `tailscale status --json`: (MagicDNS host, owner logins).
pub fn tailnet() -> (Option<String>, Vec<String>) {
    // phone access off: no tailnet host is accepted
    if !crate::config::get().mobile.tailscale {
        return (None, vec![]);
    }
    let Ok(out) = crate::platform::timeout(10).args(["tailscale", "status", "--json"]).output() else { return (None, vec![]) };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let host = v["Self"]["DNSName"].as_str().map(|s| s.trim_end_matches('.').to_string()).filter(|s| !s.is_empty());
    let uid = v["Self"]["UserID"].as_i64();
    let logins = v["User"]
        .as_object()
        .map(|m| m.values().filter(|u| uid.is_none() || u["ID"].as_i64() == uid).filter_map(|u| u["LoginName"].as_str().map(String::from)).collect())
        .unwrap_or_default();
    (host, logins)
}

/// DNS rebinding guard: the Host must name this server (loopback with our port, or the
/// tailnet name tailscale serve answers on). Any other Host is refused for every method.
pub fn host_ok(host: Option<&str>, port: u16, ts_host: Option<&str>) -> bool {
    let Some(h) = host.map(|h| h.trim().to_ascii_lowercase()) else { return false };
    let local = [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")];
    if local.contains(&h) {
        return true;
    }
    ts_host.is_some_and(|t| {
        let t = t.to_ascii_lowercase();
        h == t || h == format!("{t}:443")
    })
}

/// A request that came through tailscale serve (or any proxy) but carries no tailnet identity
/// (tagged nodes, Funnel): it must not get the local desktop's rights.
pub fn proxied_without_login(host: Option<&str>, forwarded_for: bool, login: Option<&str>, ts_host: Option<&str>) -> bool {
    if login.is_some() {
        return false;
    }
    let via_tailnet = ts_host.is_some_and(|t| host.is_some_and(|h| h.to_ascii_lowercase().starts_with(&t.to_ascii_lowercase())));
    via_tailnet || forwarded_for
}

/// How long a device request waits for approval.
pub const PENDING_TTL_MS: i64 = 86_400_000;

pub enum Access {
    /// local desktop (direct loopback)
    Local,
    /// approved phone/tablet through tailscale serve
    Device(String),
    Denied(u16, Value, Option<String>),
}

/// Decide access for one request. `set_cookie` is returned for a freshly enrolled device.
pub fn check(devices: &mut Devices, ts_login: Option<&str>, cookie: Option<&str>, user_agent: &str, allowed: &[String]) -> Access {
    let Some(login) = ts_login else { return Access::Local };
    if !allowed.iter().any(|a| a.eq_ignore_ascii_case(login)) {
        return Access::Denied(403, json!({"error": "forbidden: tailnet login not allowed", "login": login}), None);
    }
    // requests nobody approved within a day expire (the phone asks again with a new code)
    let before = devices.devices.len();
    devices.devices.retain(|d| d.approved || d.revoked || now_ms() - d.created_at < PENDING_TTL_MS);
    if devices.devices.len() != before {
        let _ = devices.save();
    }
    let token = cookie.and_then(|c| c.split(';').map(str::trim).find_map(|kv| kv.strip_prefix("adash_device="))).unwrap_or("");
    let seen = devices.by_token(token).map(|d| {
        let stale = now_ms() - d.last_seen > 60_000;
        d.last_seen = now_ms();
        stale
    });
    if seen == Some(true) {
        // persist lastSeen at most once a minute (the desktop list shows it)
        let _ = devices.save();
    }
    if let Some(d) = devices.by_token(token) {
        if d.revoked {
            return Access::Denied(403, json!({"error": "device_revoked", "deviceId": d.id}), None);
        }
        if !d.approved {
            return Access::Denied(403, json!({"error": "device_pending", "deviceId": d.id, "code": d.code, "name": d.name}), None);
        }
        return Access::Device(d.id.clone());
    }
    // the same phone asking again without its cookie (cleared, or a request that did not carry
    // it): keep its pending entry (same id/code/createdAt) and re-issue the cookie. An approved
    // device is never re-attached without its token.
    let probe = devices.enroll(login, user_agent);
    let d = match devices.devices.iter().position(|x| !x.approved && !x.revoked && x.login == probe.login && x.name == probe.name && x.id != probe.id) {
        Some(i) => {
            devices.devices.pop(); // drop the probe
            devices.devices[i].token = probe.token.clone();
            devices.devices[i].last_seen = now_ms();
            devices.devices[i].clone()
        }
        None => probe,
    };
    let _ = devices.save();
    let cookie = format!("adash_device={}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Lax", d.token);
    Access::Denied(403, json!({"error": "device_pending", "deviceId": d.id, "code": d.code, "name": d.name}), Some(cookie))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_proxy_identity_rules() {
        let ts = Some("laptop.example-tailnet.ts.net");
        assert!(
            host_ok(Some("127.0.0.1:7878"), 7878, ts) && host_ok(Some("localhost:7878"), 7878, ts) && host_ok(Some("laptop.example-tailnet.ts.net"), 7878, ts)
        );
        // DNS rebinding: an attacker's name resolving to 127.0.0.1
        assert!(!host_ok(Some("attacker.example:7878"), 7878, ts));
        assert!(!host_ok(Some("127.0.0.1:9999"), 7878, ts) && !host_ok(None, 7878, ts));
        // proxied without identity → not local
        assert!(proxied_without_login(Some("laptop.example-tailnet.ts.net"), false, None, ts));
        assert!(proxied_without_login(Some("127.0.0.1:7878"), true, None, ts));
        assert!(!proxied_without_login(Some("127.0.0.1:7878"), false, None, ts));
        assert!(!proxied_without_login(Some("laptop.example-tailnet.ts.net"), true, Some("alice@github"), ts));
    }

    #[test]
    fn local_allowed_unknown_login_denied_new_device_pending_then_approved() {
        let mut ds = Devices::default();
        let allowed = vec!["Alice@github".to_string()];
        assert!(matches!(check(&mut ds, None, None, "", &allowed), Access::Local));
        assert!(matches!(check(&mut ds, Some("evil@example.com"), None, "", &allowed), Access::Denied(403, _, None)));
        let cookie = match check(&mut ds, Some("alice@github"), None, "Mozilla/5.0 (Linux; Android 15) Chrome/140", &allowed) {
            Access::Denied(403, v, Some(c)) => {
                assert_eq!(v["error"], "device_pending");
                assert_eq!(v["code"].as_str().unwrap().len(), 6);
                c
            }
            _ => panic!("new device must be pending with a cookie"),
        };
        let token = cookie.split(';').next().unwrap().to_string();
        assert!(matches!(check(&mut ds, Some("Alice@github"), Some(&token), "", &allowed), Access::Denied(403, _, None)));
        ds.devices[0].approved = true;
        assert!(matches!(check(&mut ds, Some("Alice@github"), Some(&token), "", &allowed), Access::Device(_)));
        ds.devices[0].revoked = true;
        assert!(matches!(check(&mut ds, Some("Alice@github"), Some(&token), "", &allowed), Access::Denied(403, _, None)));
        // cookie lost while pending: same entry, new token
        let mut ds2 = Devices::default();
        let first = match check(&mut ds2, Some("Alice@github"), None, "Android Chrome", &allowed) {
            Access::Denied(_, v, _) => v,
            _ => panic!(),
        };
        let again = match check(&mut ds2, Some("Alice@github"), None, "Android Chrome", &allowed) {
            Access::Denied(_, v, Some(_)) => v,
            _ => panic!(),
        };
        assert_eq!((ds2.devices.len(), &first["deviceId"], &first["code"]), (1, &again["deviceId"], &again["code"]));
        // a guessed short token never matches
        assert!(ds.by_token("adash_device=").is_none());
    }

    #[test]
    fn unapproved_requests_expire_after_a_day() {
        let mut ds = super::Devices::default();
        let allowed = vec!["alice@github".to_string()];
        let _ = super::check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed);
        assert_eq!(ds.devices.len(), 1);
        ds.devices[0].created_at -= super::PENDING_TTL_MS + 1;
        let _ = super::check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed);
        assert_eq!(ds.devices.len(), 1, "the old request is gone; a new one takes its place");
        assert!(super::now_ms() - ds.devices[0].created_at < 60_000);
    }
}
