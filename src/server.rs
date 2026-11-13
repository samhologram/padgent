//! Local status receiver on 127.0.0.1 (port 47800 like the original's hook server).
//! Routes: POST /hook/<agent-id> with an agent's hook JSON, POST /event with
//! {"agent","session","state","title","text"}, GET /health.

use crate::model::{Core, Incoming};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn start(core: Arc<Mutex<Core>>, port: u16, wake: Arc<dyn Fn() + Send + Sync>) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            if let Ok(mut c) = core.lock() {
                c.server_status = format!(
                    "Couldn't listen on 127.0.0.1:{port} ({e}). Another copy may be running."
                );
            }
            return;
        }
    };
    if let Ok(mut c) = core.lock() {
        c.server_status = format!("Listening on 127.0.0.1:{port}");
    }
    std::thread::Builder::new()
        .name("status-receiver".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let core = core.clone();
                let wake = wake.clone();
                let _ = std::thread::Builder::new()
                    .name("status-conn".into())
                    .spawn(move || {
                        if handle(stream, &core) {
                            wake();
                        }
                    });
            }
        })
        .ok();
}

fn respond(stream: &mut TcpStream, code: u16, body: &str) {
    respond_bytes(stream, code, "application/json", body.as_bytes());
}

fn respond_bytes(stream: &mut TcpStream, code: u16, content_type: &str, body: &[u8]) {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        415 => "Unsupported Media Type",
        202 => "Accepted",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body);
}

fn handle(mut stream: TcpStream, core: &Arc<Mutex<Core>>) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if p > 16 * 1024 {
                respond(&mut stream, 400, "{\"error\":\"headers too large\"}");
                return false;
            }
            break p + 4;
        }
        if buf.len() > 16 * 1024 {
            respond(&mut stream, 400, "{\"error\":\"headers too large\"}");
            return false;
        }
    };
    let head = match std::str::from_utf8(&buf[..header_end]) {
        Ok(s) => s,
        Err(_) => {
            respond(&mut stream, 400, "{\"error\":\"invalid headers\"}");
            return false;
        }
    };
    let mut lines = head.split("\r\n");
    let parts: Vec<_> = lines.next().unwrap_or("").split(' ').collect();
    if parts.len() != 3
        || !matches!(parts[2], "HTTP/1.1" | "HTTP/1.0")
        || !parts[1].starts_with('/')
    {
        respond(&mut stream, 400, "{\"error\":\"invalid request line\"}");
        return false;
    }
    let method = parts[0].to_string();
    let (path, query) = parts[1].split_once('?').unwrap_or((parts[1], ""));
    let path = path.to_string();
    let mut headers = std::collections::HashMap::<String, String>::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            respond(&mut stream, 400, "{\"error\":\"invalid header\"}");
            return false;
        };
        if name.is_empty()
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || headers
                .insert(name.to_ascii_lowercase(), value.trim().to_string())
                .is_some()
        {
            respond(
                &mut stream,
                400,
                "{\"error\":\"invalid or duplicate header\"}",
            );
            return false;
        }
    }
    if headers.contains_key("origin") {
        respond(&mut stream, 403, "{\"error\":\"browsers are not allowed\"}");
        return false;
    }
    if headers.contains_key("transfer-encoding") {
        respond(
            &mut stream,
            400,
            "{\"error\":\"transfer encoding unsupported\"}",
        );
        return false;
    }
    let local_port = stream.local_addr().map(|a| a.port()).unwrap_or(47800);
    let valid_host = headers
        .get("host")
        .map(|h| {
            let h = h.to_ascii_lowercase();
            h == "localhost"
                || h == "127.0.0.1"
                || h == format!("localhost:{local_port}")
                || h == format!("127.0.0.1:{local_port}")
        })
        .unwrap_or(parts[2] == "HTTP/1.0");
    if !valid_host {
        respond(&mut stream, 403, "{\"error\":\"invalid host\"}");
        return false;
    }
    let length = match headers.get("content-length") {
        Some(v) if !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) => {
            match v.parse::<usize>() {
                Ok(n) if n <= 1024 * 1024 => n,
                _ => {
                    respond(&mut stream, 400, "{\"error\":\"invalid content length\"}");
                    return false;
                }
            }
        }
        None if method != "POST" => 0,
        _ => {
            respond(&mut stream, 400, "{\"error\":\"content length required\"}");
            return false;
        }
    };
    let mut body = buf[header_end..].to_vec();
    if body.len() > length {
        respond(&mut stream, 400, "{\"error\":\"unexpected body bytes\"}");
        return false;
    }
    while body.len() < length {
        match stream.read(&mut chunk[..(length - body.len()).min(8192)]) {
            Ok(0) | Err(_) => {
                respond(&mut stream, 400, "{\"error\":\"truncated body\"}");
                return false;
            }
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let tty = headers
        .get("x-tty")
        .filter(|v| v.len() < 64)
        .cloned()
        .unwrap_or_default();
    let term = headers
        .get("x-term")
        .filter(|v| v.len() < 64)
        .cloned()
        .unwrap_or_default();
    let pid = headers
        .get("x-pid")
        .filter(|v| v.len() < 12)
        .cloned()
        .unwrap_or_default();
    if method == "GET" && path.starts_with("/v1/display/keys/") {
        let segments: Vec<_> = path.split('/').collect();
        let key = match segments.as_slice() {
            ["", "v1", "display", "keys", key, "preview.png"] if key.len() == 1 => {
                key.parse::<usize>().ok().filter(|key| *key < 9)
            }
            _ => None,
        };
        let time = if query.is_empty() {
            Some(crate::model::now())
        } else {
            query
                .strip_prefix("time=")
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|t| t.is_finite() && (0.0..=86400.0).contains(t))
        };
        let (Some(key), Some(time)) = (key, time) else {
            respond(
                &mut stream,
                400,
                "{\"error\":\"invalid key or preview time\"}",
            );
            return false;
        };
        let face = match core.lock() {
            Ok(c) => c.faces()[key].clone(),
            Err(_) => {
                respond(&mut stream, 500, "{\"error\":\"controller unavailable\"}");
                return false;
            }
        };
        let font = crate::hardware::load_font();
        let pixmap = crate::hardware::render_key(&face, time, &font);
        let mut png = Vec::new();
        use image::ImageEncoder;
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        if encoder
            .write_image(
                pixmap.data(),
                pixmap.width(),
                pixmap.height(),
                image::ExtendedColorType::Rgba8,
            )
            .is_err()
        {
            respond(&mut stream, 500, "{\"error\":\"preview encoding failed\"}");
            return false;
        }
        respond_bytes(&mut stream, 200, "image/png", &png);
        return false;
    }
    if path == "/mcp" {
        if method != "POST" {
            respond(&mut stream, 405, "{\"error\":\"method not allowed\"}");
            return false;
        }
        let accepts = headers
            .get("accept")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !accepts
            .split(',')
            .any(|v| v.trim().split(';').next() == Some("application/json"))
            || !accepts
                .split(',')
                .any(|v| v.trim().split(';').next() == Some("text/event-stream"))
        {
            respond(
                &mut stream,
                406,
                "{\"error\":\"accept application/json and text/event-stream\"}",
            );
            return false;
        }
        if headers.get("content-type").map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        }) != Some(true)
        {
            respond(
                &mut stream,
                415,
                "{\"error\":\"expected application/json\"}",
            );
            return false;
        }
        if headers
            .get("mcp-protocol-version")
            .is_some_and(|v| v != crate::mcp::VERSION)
        {
            respond(
                &mut stream,
                400,
                "{\"error\":\"unsupported MCP protocol version\"}",
            );
            return false;
        }
        // An absent version denotes the older 2025-03-26 transport. This
        // stateless endpoint supports only the version negotiated at initialize.
        if !headers.contains_key("mcp-protocol-version") {
            if let Ok(request) = serde_json::from_slice::<Value>(&body) {
                if request.get("method").and_then(Value::as_str) != Some("initialize") {
                    respond(
                        &mut stream,
                        400,
                        "{\"error\":\"MCP-Protocol-Version required\"}",
                    );
                    return false;
                }
            }
        }
        match crate::mcp::handle(&body, core) {
            Some(result) => {
                respond(&mut stream, 200, &result.to_string());
                return true;
            }
            None => {
                respond(&mut stream, 202, "");
                return false;
            }
        }
    }
    if path == "/v1/display" || path.starts_with("/v1/display/") {
        let op = if method == "GET" && path == "/v1/display" {
            Some("state")
        } else if method == "POST" {
            path.strip_prefix("/v1/display/").filter(|s| {
                matches!(
                    *s,
                    "claim" | "set" | "clear" | "renew" | "release" | "events"
                )
            })
        } else {
            None
        };
        let Some(op) = op else {
            respond(&mut stream, 404, "{\"error\":\"not found\"}");
            return false;
        };
        let mut args = if method == "GET" {
            serde_json::json!({})
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Ok(v) => v,
                Err(_) => {
                    respond(&mut stream, 400, "{\"error\":\"invalid JSON\"}");
                    return false;
                }
            }
        };
        let Some(obj) = args.as_object_mut() else {
            respond(&mut stream, 400, "{\"error\":\"expected object\"}");
            return false;
        };
        obj.insert("op".into(), Value::String(op.into()));
        let command: crate::display::DisplayCommand = match serde_json::from_value(args) {
            Ok(c) => c,
            Err(e) => {
                respond(
                    &mut stream,
                    400,
                    &serde_json::json!({"error":e.to_string()}).to_string(),
                );
                return false;
            }
        };
        let result = match core.lock() {
            Ok(mut c) => c
                .display_command(command)
                .map_err(|e| (e.status, e.message)),
            Err(_) => Err((500, "controller unavailable".to_string())),
        };
        return match result {
            Ok(value) => {
                respond(&mut stream, 200, &value.to_string());
                true
            }
            Err((status, message)) => {
                respond(
                    &mut stream,
                    status,
                    &serde_json::json!({"error":message}).to_string(),
                );
                false
            }
        };
    }
    if method == "GET" && path == "/sessions" {
        // The sessions on the keypad, in key order (read-only, for scripts).
        let list: Vec<Value> = core
            .lock()
            .map(|c| {
                c.visible()
                    .iter()
                    .map(|s| serde_json::json!({ "agent": s.agent, "session": s.id, "title": s.title, "state": s.activity, "unread": s.unread, "scheduled": s.scheduled, "text": s.text }))
                    .collect()
            })
            .unwrap_or_default();
        respond(&mut stream, 200, &Value::Array(list).to_string());
        return false;
    }
    if method == "GET" && (path == "/health" || path == "/") {
        let scan = crate::local::scheduled_scan_health();
        let health = serde_json::json!({"ok": true, "app": "padgent", "accessibilityTrusted": crate::watch::trusted(),
            "displayApi": {"version":"v1", "transport":"http", "mcp":{"endpoint":"/mcp","protocolVersion":crate::mcp::VERSION,"transport":"streamable-http","stateless":true}},
            "claudeScheduledScan": {"scans":scan.scans, "directories":scan.directories, "files":scan.files,
                "records":scan.records, "eligible":scan.eligible, "permissionDenied":scan.permission_denied, "otherErrors":scan.other_errors}});
        respond(&mut stream, 200, &health.to_string());
        return false;
    }
    if method != "POST" {
        respond(&mut stream, 404, "{\"error\":\"not found\"}");
        return false;
    }
    let mut json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if let Some(o) = json.as_object_mut() {
        if !tty.is_empty() && tty != "??" {
            o.insert("_tty".into(), Value::String(tty));
        }
        if !term.is_empty() {
            o.insert("_term".into(), Value::String(term));
        }
        if !pid.is_empty() {
            if agent_is_claude(&path) {
                if let Some(name) = pid
                    .trim()
                    .parse::<i32>()
                    .ok()
                    .and_then(crate::integrations::claude_session_file)
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .filter(|n| !n.trim().is_empty())
                {
                    o.entry("title").or_insert(Value::String(name));
                }
            }
            o.insert("_pid".into(), Value::String(pid));
        }
    }
    if !json.is_object() {
        respond(&mut stream, 400, "{\"error\":\"expected a JSON object\"}");
        return false;
    }
    // POST /open {"agent","session"}: open a session as a key tap would (for scripts).
    if method == "POST" && path == "/open" {
        let agent = json
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session = json
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let found = core.lock().ok().and_then(|c| {
            let s = c
                .sessions
                .iter()
                .find(|s| s.agent == agent && s.id == session)?
                .clone();
            Some((c.agents.iter().find(|a| a.id == s.agent).cloned(), s))
        });
        match found {
            Some((def, s)) => {
                crate::mac::on_main(Box::new(move || {
                    crate::integrations::open_exact(def.as_ref(), &s)
                }));
                respond(&mut stream, 200, "{}");
            }
            None => respond(&mut stream, 404, "{\"error\":\"no such session\"}"),
        }
        return false;
    }
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (agent, event) = match segs.as_slice() {
        ["hook", agent] | ["event", agent] => (agent.to_string(), None),
        ["hook", agent, event] | ["event", agent, event] => {
            (agent.to_string(), Some(event.to_string()))
        }
        ["event"] => (
            json.get("agent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            None,
        ),
        _ => {
            respond(&mut stream, 404, "{\"error\":\"not found\"}");
            return false;
        }
    };
    if agent.is_empty() || agent.len() > 64 {
        respond(&mut stream, 400, "{\"error\":\"agent is required\"}");
        return false;
    }
    let known = match core.lock() {
        Ok(mut c) => {
            let known = c.agent(&agent).is_some();
            if known {
                let mut observation = c.decode(Incoming {
                    agent,
                    event,
                    body: json,
                });
                if matches!(segs.first(), Some(&"hook")) {
                    observation.source = crate::observation::Source::Hook;
                }
                c.observe(observation);
            }
            known
        }
        Err(_) => false,
    };
    if known {
        respond(&mut stream, 200, "{}");
    } else {
        respond(
            &mut stream,
            404,
            "{\"error\":\"unknown agent: add it in Agents or import its file\"}",
        );
    }
    known
}

fn agent_is_claude(path: &str) -> bool {
    path.trim_matches('/').split('/').nth(1) == Some("claude-code")
}
