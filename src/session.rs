//! Session state — per-session JSON files persisted on disk.
//!
//! A session stores its own *resolved* lifecycle verbs (status/attach/teardown)
//! so the lifecycle commands (`list`, `attach`, `stop`) never need to re-read
//! the recipe — which may have moved, changed, or been deleted since spawn.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Full UUID for the session.
    pub uuid: String,
    /// Short name used for the session name + CLI lookup (8 chars of uuid).
    pub name: String,
    /// Recipe identity.
    pub recipe_name: String,
    pub recipe_path: PathBuf,
    /// Repository the session was based on, if any.
    #[serde(default)]
    pub repository: Option<PathBuf>,
    /// Working directory created for this session.
    #[serde(alias = "worktree")]
    pub workdir: PathBuf,
    /// Session/runtime name (`agent-<short>`).
    #[serde(alias = "tmux_session")]
    pub session_name: String,
    /// The command run inside the session.
    #[serde(default)]
    pub command: String,
    /// Resolved liveness command — exit 0 means running. Empty ⇒ unknown.
    #[serde(default)]
    pub status_cmd: String,
    /// Resolved interactive-attach command. Empty ⇒ no attach.
    #[serde(default)]
    pub attach_cmd: String,
    /// Resolved teardown steps, run best-effort on `stop`.
    #[serde(default)]
    pub teardown: Vec<String>,
    /// RFC3339 start timestamp.
    pub started_at: String,
    /// Optional linked ticket id.
    #[serde(default)]
    pub linked_ticket: Option<String>,
}

impl Session {
    pub fn save(&self) -> Result<()> {
        let dir = state_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating state dir {}", dir.display()))?;
        let path = dir.join(format!("{}.json", self.name));
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).with_context(|| format!("writing state {}", path.display()))?;
        Ok(())
    }

    pub fn delete(&self) -> Result<()> {
        let path = state_dir()?.join(format!("{}.json", self.name));
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("removing state {}", path.display()))?;
        }
        Ok(())
    }
}

pub fn state_dir() -> Result<PathBuf> {
    let dirs = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("could not determine base dirs"))?;
    Ok(dirs.state_dir().unwrap_or(dirs.data_dir()).join("agentry"))
}

pub fn list_all() -> Result<Vec<Session>> {
    let dir = state_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(s) = serde_json::from_str::<Session>(&content) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    Ok(out)
}

pub fn find(name_or_uuid: &str) -> Result<Session> {
    for s in list_all()? {
        if s.name == name_or_uuid || s.uuid == name_or_uuid {
            return Ok(s);
        }
    }
    anyhow::bail!("no session found matching '{}'", name_or_uuid)
}

/// Generate a short identifier from a UUID (first 8 hex chars).
pub fn short_name(uuid: &str) -> String {
    uuid.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect()
}

/// Now as an RFC3339 string.
pub fn now_rfc3339() -> Result<String> {
    use time::format_description::well_known::Rfc3339;
    let now = time::OffsetDateTime::now_utc();
    now.format(&Rfc3339).context("formatting timestamp")
}

/// Root under which per-session working directories are created. Override with
/// the `AGENTRY_SESSIONS` env var; otherwise defaults to the XDG data dir
/// (`~/.local/share/agentry/sessions` on Linux).
pub fn sessions_root() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("AGENTRY_SESSIONS") {
        return Ok(PathBuf::from(v));
    }
    let dirs = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("could not determine base dirs"))?;
    Ok(dirs.data_dir().join("agentry/sessions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_name_takes_first_eight_alphanumerics() {
        assert_eq!(
            short_name("abcd1234-5678-90ab-cdef-000000000000"),
            "abcd1234"
        );
        assert_eq!(short_name("ab-cd-ef-12"), "abcdef12");
    }
}
