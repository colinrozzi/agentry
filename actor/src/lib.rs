//! agentry-actor — orchestrates local agent containers via the podman handler.
//!
//! Phase 2: HTTP routing + first podman.run integration.
//!
//!   POST /sessions  → start a hardcoded test container (proves the binding)
//!   GET  /          → liveness probe

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use packr_guest::{export, import, pack_types, GraphValue, Value};

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
    /// Empty = use the image's default command.
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
    /// -1 if the container is still running.
    #[graph(rename = "exit-code")]
    pub exit_code: i32,
}

#[derive(Clone, GraphValue)]
#[graph(crate = "packr_guest::composite_abi")]
pub struct ActorState {
    pub listener_id: String,
}

// ============================================================================
// Pack types
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
fn handle_connection(
    state: ActorState,
    connection_id: String,
) -> Result<(ActorState, ()), String> {
    if let Err(e) = tcp_activate(connection_id.clone()) {
        log(format!("[agentry-actor] activate failed: {}", e));
        return Ok((state, ()));
    }

    let body = match tcp_receive(connection_id.clone(), 8192) {
        Ok(bytes) => bytes,
        Err(e) => {
            log(format!("[agentry-actor] receive failed: {}", e));
            let _ = tcp_close(connection_id);
            return Ok((state, ()));
        }
    };

    let (status, body_text) = route(&body);
    let response = format_response(status, &body_text);
    if let Err(e) = tcp_send(connection_id.clone(), response.into_bytes()) {
        log(format!("[agentry-actor] send failed: {}", e));
    }
    let _ = tcp_close(connection_id);
    Ok((state, ()))
}

// ============================================================================
// Routing
// ============================================================================

/// Returns (status_code, body).
fn route(request: &[u8]) -> (u16, String) {
    let Some((method, path)) = parse_request_line(request) else {
        return (400, String::from("bad request line\n"));
    };

    log(format!("[agentry-actor] {} {}", method, path));

    match (method.as_str(), path.as_str()) {
        ("GET", "/") => (200, String::from("agentry-actor alive\n")),
        ("POST", "/sessions") => start_test_session(),
        ("GET", "/sessions") => list_sessions(),
        _ => (404, format!("no route for {} {}\n", method, path)),
    }
}

fn start_test_session() -> (u16, String) {
    // Phase 2: hardcoded spec just to prove the binding works.
    let spec = ContainerSpec {
        image: String::from("docker.io/library/alpine:latest"),
        name: String::from("agentry-test"),
        env: vec![],
        mounts: vec![],
        cmd: vec![String::from("sleep"), String::from("60")],
        tty: false,
        interactive: false,
    };
    match podman_run(spec) {
        Ok(id) => (201, format!("started {}\n", id)),
        Err(e) => (500, format!("podman.run failed: {}\n", e)),
    }
}

fn list_sessions() -> (u16, String) {
    match podman_list() {
        Ok(containers) => {
            let mut body = String::new();
            for c in &containers {
                body.push_str(&format!(
                    "{}\t{}\t{}\t{}\n",
                    c.name, c.status, c.image, c.id
                ));
            }
            if body.is_empty() {
                body.push_str("(no containers)\n");
            }
            (200, body)
        }
        Err(e) => (500, format!("podman.list failed: {}\n", e)),
    }
}

// ============================================================================
// HTTP helpers
// ============================================================================

fn parse_request_line(buf: &[u8]) -> Option<(String, String)> {
    // First line: METHOD SP PATH SP HTTP/1.1 CRLF
    let crlf = buf.windows(2).position(|w| w == b"\r\n")?;
    let line = core::str::from_utf8(&buf[..crlf]).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method.to_string(), path.to_string()))
}

fn format_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        body.len(),
        body
    )
}
