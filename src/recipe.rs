//! Recipe parsing and resolution.
//!
//! A recipe is a TOML file describing an agent template: name, description,
//! optional repository, and a path to a CLAUDE.md guide. The directory the
//! recipe.toml lives in is purely organizational — the tool only cares about
//! the file itself and the paths it references.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Recipe {
    /// Short identifier (`inbox-dev`, `coding`, `review`).
    pub name: String,

    /// One-line description shown in `agentry recipes list`.
    #[serde(default)]
    pub description: String,

    /// Optional default repository path. If set, used as the worktree base
    /// when spawning. If unset, must be provided at spawn time via `--repo`.
    #[serde(default)]
    pub repository: Option<PathBuf>,

    /// Relative path (from this recipe.toml) to the CLAUDE.md guide.
    pub claude_md_path: PathBuf,

    /// Internal: the path the recipe was loaded from. Used to resolve
    /// `claude_md_path` relatively.
    #[serde(skip)]
    pub source: PathBuf,
}

impl Recipe {
    /// Load a recipe from a `recipe.toml` path.
    pub fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("reading recipe {}", path.display()))?;
        let mut recipe: Recipe = toml::from_str(&content)
            .with_context(|| format!("parsing recipe {}", path.display()))?;
        recipe.source = path.to_path_buf();
        Ok(recipe)
    }

    /// Resolve `claude_md_path` against the recipe's source directory and
    /// return the absolute path.
    pub fn claude_md_abs(&self) -> Result<PathBuf> {
        let dir = self
            .source
            .parent()
            .ok_or_else(|| anyhow!("recipe source has no parent: {}", self.source.display()))?;
        Ok(dir.join(&self.claude_md_path))
    }

    /// Read the CLAUDE.md content referenced by this recipe.
    pub fn claude_md_content(&self) -> Result<String> {
        let path = self.claude_md_abs()?;
        fs::read_to_string(&path)
            .with_context(|| format!("reading CLAUDE.md at {}", path.display()))
    }
}

/// Resolve a recipe reference. If the arg looks like a path (contains a `/`
/// or ends with `.toml`), load directly. Otherwise treat as a name and look
/// it up in the search path.
pub fn resolve(reference: &str) -> Result<Recipe> {
    if reference.contains('/') || reference.ends_with(".toml") {
        let path = PathBuf::from(reference);
        Recipe::from_path(&path)
    } else {
        let path = search_path()
            .iter()
            .map(|root| root.join(reference).join("recipe.toml"))
            .find(|p| p.exists())
            .ok_or_else(|| {
                anyhow!(
                    "recipe '{}' not found in any search path: {:?}",
                    reference,
                    search_path()
                )
            })?;
        Recipe::from_path(&path)
    }
}

/// Enumerate all recipes found in the search path. Skips entries that fail to
/// parse, but returns errors for IO failures on the directory itself.
pub fn list_all() -> Result<Vec<Recipe>> {
    let mut out = Vec::new();
    for root in search_path() {
        if !root.exists() {
            continue;
        }
        let entries = fs::read_dir(&root)
            .with_context(|| format!("reading recipes dir {}", root.display()))?;
        for entry in entries.flatten() {
            let candidate = entry.path().join("recipe.toml");
            if candidate.is_file() {
                if let Ok(recipe) = Recipe::from_path(&candidate) {
                    out.push(recipe);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Default search path for named recipe lookups. Override with the
/// `AGENTRY_RECIPES` env var (colon-separated, like `$PATH`).
pub fn search_path() -> Vec<PathBuf> {
    if let Ok(env) = std::env::var("AGENTRY_RECIPES") {
        return env.split(':').map(PathBuf::from).collect();
    }
    let mut roots = Vec::new();
    if let Some(dirs) = directories::BaseDirs::new() {
        roots.push(dirs.config_dir().join("agentry/recipes"));
    }
    roots
}
