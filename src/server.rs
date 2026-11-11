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
                c.server_status = format!("Couldn't listen on 127.0.0.1:{port} ({e}). Another copy may be running.");
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
                let _ = std::thread::Builder::new().name("status-conn".into()).spawn(move || {
                    if handle(stream, &core) {
                        wake();
                    }
                });
            }
        })
        .ok();
}

fn respond(stream: &mut TcpStream, code: u16, body: &str) {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn handle(mut stream: TcpStream, core: &Arc<Mutex<Core>>) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    let header_end;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = p + 4;
            break;
        }
        if buf.len() > 64 * 1024 {
            return false;
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    let mut length = 0usize;
    let mut tty = String::new();
    let mut term = String::new();
    let mut pid = String::new();
    for l in lines {
        let lower = l.to_ascii_lowercase();
        if let Some(i) = l.find(':') {
            let (k, v) = (l[..i].trim().to_ascii_lowercase(), l[i + 1..].trim().to_string());
            if k == "x-tty" && v.len() < 64 {
                tty = v;
            } else if k == "x-pid" && v.len() < 12 {
                pid = v;
            } else if k == "x-term" && v.len() < 64 {
                term = v;
            }
        }
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
        // Web pages may not post here: only local programs (hooks, scripts) can.
        if lower.starts_with("origin:") {
            respond(&mut stream, 403, "{\"error\":\"browsers are not allowed\"}");
            return false;
        }
    }
    if length > 1024 * 1024 {
        respond(&mut stream, 400, "{\"error\":\"too large\"}");
        return false;
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    if method == "GET" && path == "/sessions" {
        // The sessions on the keypad, in key order (read-only, for scripts).
        let list: Vec<Value> = core
            .lock()
            .map(|c| {
                c.visible()
                    .iter()
                    .map(|s| serde_json::json!({ "agent": s.agent, "session": s.id, "title": s.title, "state": s.activity, "unread": s.unread, "text": s.text }))
                    .collect()
            })
            .unwrap_or_default();
        respond(&mut stream, 200, &Value::Array(list).to_string());
        return false;
    }
    if method == "GET" && (path == "/health" || path == "/") {
        respond(&mut stream, 200, "{\"ok\":true,\"app\":\"agentpad\"}");
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
        let agent = json.get("agent").and_then(Value::as_str).unwrap_or("").to_string();
        let session = json.get("session").and_then(Value::as_str).unwrap_or("").to_string();
        let found = core.lock().ok().and_then(|c| {
            let s = c.sessions.iter().find(|s| s.agent == agent && s.id == session)?.clone();
            Some((c.agents.iter().find(|a| a.id == s.agent).cloned(), s))
        });
        match found {
            Some((def, s)) => {
                crate::mac::on_main(Box::new(move || crate::integrations::open_exact(def.as_ref(), &s)));
                respond(&mut stream, 200, "{}");
            }
            None => respond(&mut stream, 404, "{\"error\":\"no such session\"}"),
        }
        return false;
    }
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (agent, event) = match segs.as_slice() {
        ["hook", agent] | ["event", agent] => (agent.to_string(), None),
        ["hook", agent, event] | ["event", agent, event] => (agent.to_string(), Some(event.to_string())),
        ["event"] => (json.get("agent").and_then(Value::as_str).unwrap_or("").to_string(), None),
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
                c.apply(Incoming { agent, event, body: json });
            }
            known
        }
        Err(_) => false,
    };
    if known {
        respond(&mut stream, 200, "{}");
    } else {
        respond(&mut stream, 404, "{\"error\":\"unknown agent: add it in Agents or import its file\"}");
    }
    known
}

fn agent_is_claude(path: &str) -> bool {
    path.trim_matches('/').split('/').nth(1) == Some("claude-code")
}
