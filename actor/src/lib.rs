//! agentry-actor — orchestrates local agent containers via the podman handler.
//!
//! HTTP API on 127.0.0.1:8090:
//!   GET    /                       — liveness probe (text/plain)
//!   POST   /sessions               — start a container from a spec (JSON body)
//!   GET    /sessions               — list all containers (JSON array)
//!   GET    /sessions/<name>        — single container (JSON object, 404 if missing)
//!   DELETE /sessions/<name>        — stop + remove (idempotent, 204 on success)

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, GraphValue, Value};
use serde::Deserialize;

packr_guest::setup_guest!();

// ============================================================================
// Records mirroring podman.pact
// ============================================================================

#[derive(Clone, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct MountSpec {
    pub source: String,
    pub target: String,
    #[graph(rename = "read-only")]
    pub read_only: bool,
}

#[derive(Clone, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct ContainerSpec {
    pub image: String,
    pub name: String,
    pub env: Vec<(String, String)>,
    pub mounts: Vec<MountSpec>,
    pub cmd: Vec<String>,
    pub tty: bool,
    pub interactive: bool,
}

#[derive(Clone, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    #[graph(rename = "exit-code")]
    pub exit_code: i32,
}

#[derive(Clone, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct ActorState {
    pub listener_id: String,
}

// ============================================================================
// JSON request body shape (deserialized from POST /sessions)
// ============================================================================

#[derive(Deserialize)]
struct SessionRequest<'a> {
    image: &'a str,
    name: &'a str,
    #[serde(default)]
    env: Vec<(String, String)>,
    #[serde(default)]
    mounts: Vec<MountReq<'a>>,
    #[serde(default)]
    cmd: Vec<String>,
    #[serde(default)]
    tty: bool,
    #[serde(default)]
    interactive: bool,
}

#[derive(Deserialize)]
struct MountReq<'a> {
    source: &'a str,
    target: &'a str,
    #[serde(default, rename = "read_only")]
    read_only: bool,
}

// ============================================================================
// pack_types
// ============================================================================

pack_types!(file = "agentry-actor.types");

#[import(module = "theater:simple/runtime", name = "log")]
fn log(msg: String);

#[import(module = "theater:simple/tcp", name = "listen")]
fn tcp_listen(address: String) -> Result<String, String>;

#[import(module = "theater:simple/tcp", name = "activate")]
fn tcp_activate(connection_id: String) -> Result<(), String>;

#[import(module = "theater:simple/tcp", name = "receive")]
fn tcp_receive(connection_id: String, max_bytes: u32) -> Result<Vec<u8>, String>;

#[import(module = "theater:simple/tcp", name = "send")]
fn tcp_send(connection_id: String, data: Vec<u8>) -> Result<u64, String>;

#[import(module = "theater:simple/tcp", name = "close")]
fn tcp_close(connection_id: String) -> Result<(), String>;

#[import(module = "theater:simple/podman", name = "run")]
fn podman_run(spec: ContainerSpec) -> Result<String, String>;

#[import(module = "theater:simple/podman", name = "stop")]
fn podman_stop(name: String) -> Result<(), String>;

#[import(module = "theater:simple/podman", name = "rm")]
fn podman_rm(name: String, force: bool) -> Result<(), String>;

#[import(module = "theater:simple/podman", name = "list")]
fn podman_list() -> Result<Vec<ContainerInfo>, String>;

// ============================================================================
// Constants
// ============================================================================

const LISTEN_ADDR: &str = "127.0.0.1:8090";

// ============================================================================
// Lifecycle
// ============================================================================

#[export(name = "theater:simple/actor.init")]
fn init(_state: Value) -> Result<(ActorState, ()), String> {
    log(String::from("[agentry-actor] init"));
    let listener_id =
        tcp_listen(String::from(LISTEN_ADDR)).map_err(|e| format!("listen failed: {}", e))?;
    log(format!(
        "[agentry-actor] listening on {} (id={})",
        LISTEN_ADDR, listener_id
    ));
    Ok((ActorState { listener_id }, ()))
}

#[export(name = "theater:simple/tcp-client.handle-connection")]
fn handle_connection(state: ActorState, connection_id: String) -> Result<(ActorState, ()), String> {
    if let Err(e) = tcp_activate(connection_id.clone()) {
        log(format!("[agentry-actor] activate failed: {}", e));
        return Ok((state, ()));
    }

    let bytes = match tcp_receive(connection_id.clone(), 16384) {
        Ok(b) => b,
        Err(e) => {
            log(format!("[agentry-actor] receive failed: {}", e));
            let _ = tcp_close(connection_id);
            return Ok((state, ()));
        }
    };

    let (status, body, content_type) = route(&bytes);
    let response = format_response(status, &body, content_type);
    if let Err(e) = tcp_send(connection_id.clone(), response.into_bytes()) {
        log(format!("[agentry-actor] send failed: {}", e));
    }
    let _ = tcp_close(connection_id);
    Ok((state, ()))
}

// ============================================================================
// Routing
// ============================================================================

/// Returns (status_code, body, content_type).
fn route(request: &[u8]) -> (u16, String, &'static str) {
    let Some((method, path, body_offset)) = parse_request_head(request) else {
        return (400, String::from("bad request\n"), "text/plain");
    };

    log(format!("[agentry-actor] {} {}", method, path));

    // Static routes first
    match (method.as_str(), path.as_str()) {
        ("GET", "/") => return (200, String::from("agentry-actor alive\n"), "text/plain"),
        ("GET", "/sessions") => return list_sessions(),
        ("POST", "/sessions") => {
            let body_bytes = &request[body_offset..];
            return start_session(body_bytes);
        }
        _ => {}
    }

    // /sessions/<name>
    if let Some(name) = path.strip_prefix("/sessions/") {
        if name.is_empty() {
            return (
                400,
                json_error("missing session name in path"),
                "application/json",
            );
        }
        match method.as_str() {
            "GET" => return show_session(name),
            "DELETE" => return delete_session(name),
            _ => {}
        }
    }

    (
        405,
        json_error(&format!("method {} not allowed on {}", method, path)),
        "application/json",
    )
}

// ============================================================================
// Endpoint handlers
// ============================================================================

fn start_session(body: &[u8]) -> (u16, String, &'static str) {
    let req: SessionRequest = match serde_json_core::from_slice(body) {
        Ok((r, _)) => r,
        Err(e) => {
            return (
                400,
                json_error(&format!("invalid JSON body: {:?}", e)),
                "application/json",
            )
        }
    };

    let spec = ContainerSpec {
        image: req.image.to_string(),
        name: req.name.to_string(),
        env: req.env,
        mounts: req
            .mounts
            .into_iter()
            .map(|m| MountSpec {
                source: m.source.to_string(),
                target: m.target.to_string(),
                read_only: m.read_only,
            })
            .collect(),
        cmd: req.cmd,
        tty: req.tty,
        interactive: req.interactive,
    };

    let name = spec.name.clone();
    match podman_run(spec) {
        Ok(id) => {
            let body = format!(
                "{{\"name\":\"{}\",\"container_id\":\"{}\"}}\n",
                escape_json(&name),
                escape_json(&id)
            );
            (201, body, "application/json")
        }
        Err(e) => (
            500,
            json_error(&format!("podman.run failed: {}", e)),
            "application/json",
        ),
    }
}

fn list_sessions() -> (u16, String, &'static str) {
    let containers = match podman_list() {
        Ok(cs) => cs,
        Err(e) => {
            return (
                500,
                json_error(&format!("podman.list failed: {}", e)),
                "application/json",
            )
        }
    };

    let mut body = String::from("[");
    for (i, c) in containers.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&container_info_json(c));
    }
    body.push(']');
    body.push('\n');
    (200, body, "application/json")
}

fn show_session(name: &str) -> (u16, String, &'static str) {
    let containers = match podman_list() {
        Ok(cs) => cs,
        Err(e) => {
            return (
                500,
                json_error(&format!("podman.list failed: {}", e)),
                "application/json",
            )
        }
    };
    match containers.iter().find(|c| c.name == name) {
        Some(c) => (200, container_info_json(c) + "\n", "application/json"),
        None => (404, json_error("no such session"), "application/json"),
    }
}

fn delete_session(name: &str) -> (u16, String, &'static str) {
    // Best-effort stop, then force-remove. podman host functions are
    // already idempotent on "no such container", so this composes cleanly.
    if let Err(e) = podman_stop(name.to_string()) {
        return (
            500,
            json_error(&format!("podman.stop failed: {}", e)),
            "application/json",
        );
    }
    if let Err(e) = podman_rm(name.to_string(), true) {
        return (
            500,
            json_error(&format!("podman.rm failed: {}", e)),
            "application/json",
        );
    }
    (204, String::new(), "application/json")
}

// ============================================================================
// JSON helpers (hand-rolled — bodies are simple, no nested escaping needed)
// ============================================================================

fn container_info_json(c: &ContainerInfo) -> String {
    format!(
        "{{\"name\":\"{}\",\"image\":\"{}\",\"status\":\"{}\",\"exit_code\":{},\"container_id\":\"{}\"}}",
        escape_json(&c.name),
        escape_json(&c.image),
        escape_json(&c.status),
        c.exit_code,
        escape_json(&c.id),
    )
}

fn json_error(msg: &str) -> String {
    format!("{{\"error\":\"{}\"}}\n", escape_json(msg))
}

/// Escape just the JSON-required chars in string values. We don't expect
/// control chars in the inputs we care about; this is enough.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

// ============================================================================
// HTTP parsing
// ============================================================================

/// Parse the request line + skip headers. Returns (method, path, body_offset).
fn parse_request_head(buf: &[u8]) -> Option<(String, String, usize)> {
    // Request line ends at the first \r\n.
    let crlf = buf.windows(2).position(|w| w == b"\r\n")?;
    let line = core::str::from_utf8(&buf[..crlf]).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    // Body begins after the blank line (\r\n\r\n).
    let body_offset = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());

    Some((method, path, body_offset))
}

fn format_response(status: u16, body: &str, content_type: &str) -> String {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    if status == 204 {
        format!("HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
    } else {
        format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            reason,
            content_type,
            body.len(),
            body
        )
    }
}
