//! Wire protocol for the agentry control socket.
//!
//! Transport: a Unix stream socket, one newline-delimited JSON request per
//! connection, one JSON response back. Minimal on purpose — no RPC framework.

use crate::session::Session;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped when the request/response shape changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// A request from a client to the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version the client speaks.
    pub v: u32,
    /// Operation name, e.g. `session.list`, `session.start`.
    pub op: String,
    /// Operation arguments (shape depends on `op`).
    #[serde(default)]
    pub args: serde_json::Value,
}

/// A response from the daemon to a client.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(data: serde_json::Value) -> Self {
        Response {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Response {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// A session plus its computed liveness, as returned over the wire.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub session: Session,
    /// "running" | "stale" | "unknown".
    pub status: String,
}

/// Returned by `session.start` when the recipe's runtime is `foreground`: the
/// daemon has already run `setup`, so the client just runs `command` in
/// `workdir` (its terminal) and `teardown` on exit.
#[derive(Debug, Serialize, Deserialize)]
pub struct ForegroundPlan {
    pub name: String,
    pub recipe_name: String,
    pub command: String,
    pub workdir: PathBuf,
    pub teardown: Vec<String>,
}

/// The control socket path: `$AGENTRY_SOCKET`, else `$XDG_RUNTIME_DIR/agentry/
/// agentry.sock`, else `~/.local/state/agentry/agentry.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENTRY_SOCKET") {
        return PathBuf::from(p);
    }
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("agentry/agentry.sock");
    }
    let base = directories::BaseDirs::new()
        .map(|d| d.state_dir().unwrap_or(d.data_dir()).to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("agentry/agentry.sock")
}
