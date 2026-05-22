//! Session state — per-session JSON files persisted on disk.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Full UUID for the session.
    pub uuid: String,
    /// Short name used for tmux session + CLI lookup (8 hex chars of uuid).
    pub name: String,
    /// Recipe identity.
    pub recipe_name: String,
    pub recipe_path: PathBuf,
    /// Repository the worktree was based on.
    pub repository: PathBuf,
    /// Worktree path created for this session.
    pub worktree: PathBuf,
    /// tmux session name (`agent-<short>`).
    pub tmux_session: String,
    /// RFC3339 start timestamp.
    pub started_at: String,
    /// Optional linked ticket id.
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

/// Convenience for callers wanting to derive paths from a path arg or name.
#[allow(dead_code)]
pub fn worktree_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(Path::new(&home).join("work/agentry-sessions"))
}
