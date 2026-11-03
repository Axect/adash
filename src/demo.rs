//! `adash demo`: the dashboard on built-in sample data, without tmux or agents. The web app's
//! fixture mode (`?fixture=1`) reads `web/fixture*.json` and simulates activity in the browser;
//! this server only hands out the embedded web files and refuses every API call.
use crate::server::{mime_for, WEB};
use crate::util::Res;
use tiny_http::{Header, Response, Server};

pub fn serve(port: u16) -> Res<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    println!("adash demo: http://127.0.0.1:{port}/  (sample data; nothing is read from or sent to agents)");
    for req in server.incoming_requests() {
        let url = req.url().split('?').next().unwrap_or("/").to_string();
        let index = url == "/" || url == "/index.html";
        let r = if index && !req.url().contains("fixture=1") {
            // English unless the browser prefers Korean
            let ko = req.headers().iter().any(|h| h.field.equiv("Accept-Language") && h.value.as_str().trim_start().starts_with("ko"));
            let to = if ko { "/?fixture=1" } else { "/?fixture=1&lang=en" };
            Response::from_string("").with_status_code(302).with_header(Header::from_bytes("Location", to).unwrap())
        } else if url.starts_with("/api/") {
            let code = if req.method() == &tiny_http::Method::Get { 404 } else { 403 };
            Response::from_string(r#"{"error":"demo mode: no live data"}"#)
                .with_status_code(code)
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        } else {
            let path = if index { "index.html" } else { url.trim_start_matches('/') };
            match WEB.get_file(path) {
                Some(f) => Response::from_data(f.contents().to_vec())
                    .with_header(Header::from_bytes("Content-Type", mime_for(path).unwrap_or("application/octet-stream")).unwrap()),
                None => Response::from_string("not found").with_status_code(404),
            }
        };
        let _ = req.respond(r);
    }
    Ok(())
}
