//! Phase C: phone access through `tailscale serve` (HTTPS, tailnet only).
//! The server still binds 127.0.0.1; requests proxied by Tailscale carry `Tailscale-User-Login`.
//! Rules: direct loopback requests (no Tailscale headers) are the local desktop and allowed;
//! proxied requests need an allowed tailnet login AND an approved device (cookie token) —
//! a new device gets a 6-digit code and waits until the user approves it on the desktop.
//! An approved device is also known by its Tailscale node (from `tailscale whois` on the
//! client address tailscale serve forwards): the same phone reaching the dashboard without the
//! cookie (a home-screen app and a browser tab keep separate cookies; cleared site data) is let
//! in and gets a new cookie instead of a new approval code.
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
    /// more cookies of the same device (another browser or app on the same node), newest last
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tokens: Vec<String>,
    /// Tailscale node (StableID) the device was seen on, filled on its next visit
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub node_name: Option<String>,
}

/// A tailnet node as `tailscale whois` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub login: String,
}

/// The node behind a request proxied by tailscale serve: the last `X-Forwarded-For` address
/// (the one serve adds), looked up with `whois`, and only when the node's login is the
/// `Tailscale-User-Login` serve sent. Anything else (no header, a non-tailnet address, a
/// mismatching login) gives no node, and the request falls back to the cookie.
pub fn node_from(forwarded_for: Option<&str>, login: Option<&str>, whois: &dyn Fn(&str) -> Option<Node>) -> Option<Node> {
    let login = login?;
    let ip: std::net::IpAddr = forwarded_for?.rsplit(',').next()?.trim().parse().ok()?;
    let tailnet = match ip {
        std::net::IpAddr::V4(v) => v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]),
        std::net::IpAddr::V6(v) => v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    };
    if !tailnet {
        return None;
    }
    whois(&ip.to_string()).filter(|n| n.login.eq_ignore_ascii_case(login))
}

/// `tailscale whois --json <ip>`, cached ten minutes per address.
pub fn whois(ip: &str) -> Option<Node> {
    use crate::util::LockExt;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, (i64, Option<Node>)>>> = std::sync::LazyLock::new(Default::default);
    if let Some((at, n)) = CACHE.plock().get(ip) {
        if now_ms() - at < 600_000 {
            return n.clone();
        }
    }
    let n = crate::platform::timeout(5)
        .args(["tailscale", "whois", "--json", ip])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| {
            Some(Node {
                id: v["Node"]["StableID"].as_str()?.to_string(),
                name: v["Node"]["ComputedName"].as_str().or_else(|| v["Node"]["Hostinfo"]["Hostname"].as_str()).unwrap_or("").to_string(),
                login: v["UserProfile"]["LoginName"].as_str()?.to_string(),
            })
        });
    CACHE.plock().insert(ip.to_string(), (now_ms(), n.clone()));
    n
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
        self.devices.iter_mut().find(|d| d.token == token || d.tokens.iter().any(|t| t == token))
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
            tokens: vec![],
            node_id: None,
            node_name: None,
        };
        self.devices.push(d.clone());
        d
    }

    pub fn listing(&self) -> Value {
        let list: Vec<Value> = self
            .devices
            .iter()
            .map(|d| json!({"id": d.id, "name": d.name, "login": d.login, "node": d.node_name, "approved": d.approved && !d.revoked, "revoked": d.revoked, "pending": !d.approved && !d.revoked, "lastSeen": d.last_seen, "createdAt": d.created_at, "code": d.code}))
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
    /// approved phone/tablet through tailscale serve (a new cookie when it came in by its node)
    Device(String, Option<String>),
    Denied(u16, Value, Option<String>),
}

/// Decide access for one request. `set_cookie` is returned for a freshly enrolled device.
pub fn check(devices: &mut Devices, ts_login: Option<&str>, cookie: Option<&str>, user_agent: &str, allowed: &[String], node: Option<&Node>) -> Access {
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
            if let Some(n) = node {
                if d.node_id.is_none() {
                    d.node_id = Some(n.id.clone());
                    d.node_name = Some(n.name.clone());
                }
            }
            return Access::Denied(403, json!({"error": "device_pending", "deviceId": d.id, "code": d.code, "name": d.name}), None);
        }
        let id = d.id.clone();
        if let Some(n) = node {
            if d.node_id.as_deref() != Some(n.id.as_str()) {
                d.node_id = Some(n.id.clone());
                d.node_name = Some(n.name.clone());
                merge_node(devices, &id, &n.id);
                let _ = devices.save();
            }
        }
        return Access::Device(id, None);
    }
    // an approved device on the same Tailscale node: let it in and give it a cookie of its own
    if let Some(n) = node {
        if let Some(d) = devices.devices.iter_mut().find(|d| d.approved && !d.revoked && d.node_id.as_deref() == Some(n.id.as_str())) {
            let t = random_hex(24);
            d.tokens.push(t.clone());
            if d.tokens.len() > 8 {
                d.tokens.remove(0);
            }
            d.last_seen = now_ms();
            d.node_name = Some(n.name.clone());
            let id = d.id.clone();
            let _ = devices.save();
            return Access::Device(id, Some(format!("adash_device={t}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Lax")));
        }
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
    if let (Some(n), Some(x)) = (node, devices.devices.iter_mut().find(|x| x.id == d.id)) {
        x.node_id = Some(n.id.clone());
        x.node_name = Some(n.name.clone());
    }
    let _ = devices.save();
    let cookie = format!("adash_device={}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Lax", d.token);
    Access::Denied(403, json!({"error": "device_pending", "deviceId": d.id, "code": d.code, "name": d.name}), Some(cookie))
}

/// Several approved entries of one node (enrolled before nodes were recorded): keep `keep`,
/// fold the others' cookies into it and drop them.
fn merge_node(devices: &mut Devices, keep: &str, node_id: &str) {
    let others: Vec<Device> =
        devices.devices.iter().filter(|d| d.id != keep && d.approved && !d.revoked && d.node_id.as_deref() == Some(node_id)).cloned().collect();
    if others.is_empty() {
        return;
    }
    devices.devices.retain(|d| !others.iter().any(|o| o.id == d.id));
    if let Some(k) = devices.devices.iter_mut().find(|d| d.id == keep) {
        for o in others {
            k.tokens.push(o.token);
            k.tokens.extend(o.tokens);
            k.created_at = k.created_at.min(o.created_at);
        }
        let n = k.tokens.len();
        if n > 8 {
            k.tokens.drain(..n - 8);
        }
    }
}

/// Revoke a device and every other entry of the same Tailscale node; returns the ids revoked.
pub fn revoke_node(devices: &mut Devices, id: &str) -> Vec<String> {
    let node = devices.devices.iter().find(|d| d.id == id).and_then(|d| d.node_id.clone());
    let mut out = vec![];
    for d in devices.devices.iter_mut().filter(|d| d.id == id || (node.is_some() && d.node_id == node)) {
        d.revoked = true;
        out.push(d.id.clone());
    }
    out
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
        assert!(matches!(check(&mut ds, None, None, "", &allowed, None), Access::Local));
        assert!(matches!(check(&mut ds, Some("evil@example.com"), None, "", &allowed, None), Access::Denied(403, _, None)));
        let cookie = match check(&mut ds, Some("alice@github"), None, "Mozilla/5.0 (Linux; Android 15) Chrome/140", &allowed, None) {
            Access::Denied(403, v, Some(c)) => {
                assert_eq!(v["error"], "device_pending");
                assert_eq!(v["code"].as_str().unwrap().len(), 6);
                c
            }
            _ => panic!("new device must be pending with a cookie"),
        };
        let token = cookie.split(';').next().unwrap().to_string();
        assert!(matches!(check(&mut ds, Some("Alice@github"), Some(&token), "", &allowed, None), Access::Denied(403, _, None)));
        ds.devices[0].approved = true;
        assert!(matches!(check(&mut ds, Some("Alice@github"), Some(&token), "", &allowed, None), Access::Device(_, None)));
        ds.devices[0].revoked = true;
        assert!(matches!(check(&mut ds, Some("Alice@github"), Some(&token), "", &allowed, None), Access::Denied(403, _, None)));
        // cookie lost while pending: same entry, new token
        let mut ds2 = Devices::default();
        let first = match check(&mut ds2, Some("Alice@github"), None, "Android Chrome", &allowed, None) {
            Access::Denied(_, v, _) => v,
            _ => panic!(),
        };
        let again = match check(&mut ds2, Some("Alice@github"), None, "Android Chrome", &allowed, None) {
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
        let _ = super::check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed, None);
        assert_eq!(ds.devices.len(), 1);
        ds.devices[0].created_at -= super::PENDING_TTL_MS + 1;
        let _ = super::check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed, None);
        assert_eq!(ds.devices.len(), 1, "the old request is gone; a new one takes its place");
        assert!(super::now_ms() - ds.devices[0].created_at < 60_000);
    }

    #[test]
    fn the_same_tailscale_node_comes_back_without_its_cookie() {
        let mut ds = super::Devices::default();
        let allowed = vec!["alice@github".to_string()];
        let phone = Node { id: "nPHONE".into(), name: "s26-ultra".into(), login: "alice@github".into() };
        let tablet = Node { id: "nTAB".into(), name: "fold".into(), login: "alice@github".into() };
        let _ = check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed, Some(&phone));
        assert_eq!(ds.devices[0].node_id.as_deref(), Some("nPHONE"));
        ds.devices[0].approved = true;
        // the home-screen app: no cookie, same node → in, with a cookie of its own
        let c = match check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed, Some(&phone)) {
            Access::Device(id, Some(c)) => {
                assert_eq!(id, ds.devices[0].id);
                c
            }
            _ => panic!("same node must pass"),
        };
        let token = c.split(';').next().unwrap().to_string();
        assert!(matches!(check(&mut ds, Some("alice@github"), Some(&token), "", &allowed, None), Access::Device(_, None)));
        assert_eq!(ds.devices.len(), 1, "no new approval request");
        // another node asks for approval
        assert!(matches!(check(&mut ds, Some("alice@github"), None, "Android Chrome", &allowed, Some(&tablet)), Access::Denied(403, _, Some(_))));
        // revoking the device revokes the node: no way back in without a new approval
        let id = ds.devices[0].id.clone();
        revoke_node(&mut ds, &id);
        assert!(matches!(check(&mut ds, Some("alice@github"), Some(&token), "", &allowed, Some(&phone)), Access::Denied(403, _, None)));
    }

    #[test]
    fn node_needs_a_tailnet_address_and_the_login_tailscale_serve_sent() {
        let who = |ip: &str| (ip == "100.101.242.124").then(|| Node { id: "nP".into(), name: "s26-ultra".into(), login: "Alice@github".into() });
        assert_eq!(node_from(Some("100.101.242.124"), Some("alice@github"), &who).map(|n| n.id), Some("nP".into()));
        // the address serve appends is the last one; a forged first entry changes nothing
        assert_eq!(node_from(Some("100.101.242.124, 100.99.1.1"), Some("alice@github"), &who), None);
        assert!(node_from(Some("8.8.8.8, 100.101.242.124"), Some("alice@github"), &who).is_some());
        assert_eq!(node_from(Some("100.101.242.124"), Some("mallory@github"), &who), None, "login mismatch");
        assert_eq!(node_from(Some("192.168.0.5"), Some("alice@github"), &who), None, "not a tailnet address");
        assert_eq!(node_from(Some("100.101.242.124"), None, &who), None, "no tailscale serve identity: no node");
    }

    #[test]
    fn older_entries_of_one_node_are_merged() {
        let mut ds = super::Devices::default();
        let allowed = vec!["alice@github".to_string()];
        for _ in 0..2 {
            let d = ds.enroll("alice@github", "Android Chrome");
            let i = ds.devices.iter().position(|x| x.id == d.id).unwrap();
            ds.devices[i].approved = true;
        }
        let (t0, t1) = (ds.devices[0].token.clone(), ds.devices[1].token.clone());
        let phone = Node { id: "nP".into(), name: "s26-ultra".into(), login: "alice@github".into() };
        let c0 = format!("adash_device={t0}");
        let c1 = format!("adash_device={t1}");
        let _ = check(&mut ds, Some("alice@github"), Some(&c0), "", &allowed, Some(&phone));
        let _ = check(&mut ds, Some("alice@github"), Some(&c1), "", &allowed, Some(&phone));
        assert_eq!(ds.devices.len(), 1, "one node, one entry");
        assert!(matches!(check(&mut ds, Some("alice@github"), Some(&c0), "", &allowed, None), Access::Device(_, None)));
        assert!(matches!(check(&mut ds, Some("alice@github"), Some(&c1), "", &allowed, None), Access::Device(_, None)));
    }
}
