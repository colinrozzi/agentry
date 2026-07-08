//! Recipe parsing, resolution, and instantiation planning.
//!
//! A recipe is the instantiation document for an agent — its Dockerfile. It's a
//! TOML file describing an agent template: identity (name, description, a
//! CLAUDE.md guide) plus an optional, fully-declarative lifecycle expressed as
//! shell steps: `setup`, `launch`, `status`, `attach`, `teardown`.
//!
//! agentry itself knows nothing about jj or tmux. Those live only in the
//! built-in default template below: a recipe that declares no lifecycle verbs
//! inherits today's behavior (jj workspace + tmux session running `claude`), so
//! existing recipes keep working untouched. A recipe that *does* declare a verb
//! overrides just that verb — a `dir` workspace, a cloud runtime, whatever.
//!
//! Steps are templated with `{var}` placeholders and run through `sh -c`.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The command run inside the session (inside tmux, for the default runtime).
const DEFAULT_COMMAND: &str = "claude";
/// Where the session's working directory lives.
const DEFAULT_WORKDIR: &str = "{sessions_root}/{uuid}";
/// Default workspace provisioning: a sibling jj workspace on `main`, plus a
/// copy of the recipe's CLAUDE.md into the workspace root.
const DEFAULT_SETUP: &[&str] = &[
    "jj -R {repo} workspace add -r main --name {session} {workdir}",
    "cp {claude_md} {workdir}/CLAUDE.md",
];
/// Default runtime: a detached tmux session running `{command}` in `{workdir}`.
const DEFAULT_LAUNCH: &str = "tmux new-session -d -s {session} -c {workdir} {command}";
/// Default liveness check: exit 0 if the tmux session exists.
const DEFAULT_STATUS: &str = "tmux has-session -t {session}";
/// Default interactive attach.
const DEFAULT_ATTACH: &str = "tmux attach -t {session}";
/// Default teardown: kill the tmux session, forget the jj workspace, remove dir.
const DEFAULT_TEARDOWN: &[&str] = &[
    "tmux kill-session -t {session}",
    "jj -R {repo} workspace forget {session}",
    "rm -rf {workdir}",
];

/// The fixed vocabulary of template variables. A `{name}` whose name is in this
/// set is substituted (or errors, if referenced but unset); any other `{...}` —
/// e.g. shell `${HOME}` or `awk '{print}'` — is left untouched.
const KNOWN_VARS: &[&str] = &[
    "uuid",
    "short",
    "session",
    "workdir",
    "sessions_root",
    "recipe_dir",
    "claude_md",
    "repo",
    "command",
];

#[derive(Debug, Deserialize)]
pub struct Recipe {
    /// Short identifier (`inbox-dev`, `coding`, `onboarding-agent`).
    pub name: String,

    /// One-line description shown in `agentry recipes list`.
    #[serde(default)]
    pub description: String,

    /// Optional default repository path. Feeds the `{repo}` template variable.
    /// Overridable at spawn time via `--repo`. Unset is fine for recipes whose
    /// steps never reference `{repo}` (e.g. a `dir`-workspace onboarding agent).
    #[serde(default)]
    pub repository: Option<PathBuf>,

    /// Optional relative path (from this recipe.toml) to a CLAUDE.md guide.
    /// Feeds the `{claude_md}` template variable (resolved to an absolute path).
    #[serde(default)]
    pub claude_md_path: Option<PathBuf>,

    // ---- Lifecycle (all optional; unset ⇒ the built-in default template) ----
    /// Command run inside the session. Default `claude`.
    #[serde(default)]
    pub command: Option<String>,
    /// The session's working directory. Default `{sessions_root}/{uuid}`.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Steps to provision the workspace. Default: jj workspace + copy CLAUDE.md.
    #[serde(default)]
    pub setup: Option<Vec<String>>,
    /// Step to start the (detached) runtime. Default: tmux new-session.
    #[serde(default)]
    pub launch: Option<String>,
    /// Liveness check — exit 0 means running. Default: tmux has-session.
    #[serde(default)]
    pub status: Option<String>,
    /// Interactive attach. Default: tmux attach.
    #[serde(default)]
    pub attach: Option<String>,
    /// Steps to stop the runtime and deprovision. Default: kill tmux, forget
    /// jj workspace, remove dir. Resolved at start and stored in the session's
    /// state file, so `stop` is self-contained.
    #[serde(default)]
    pub teardown: Option<Vec<String>>,

    /// Internal: the path the recipe was loaded from.
    #[serde(skip)]
    pub source: PathBuf,
}

/// A fully-resolved instantiation plan: every template variable substituted,
/// ready to execute. Produced by [`Recipe::plan`].
#[derive(Debug, Clone)]
pub struct Plan {
    pub session_name: String,
    pub workdir: PathBuf,
    pub command: String,
    pub setup: Vec<String>,
    pub launch: String,
    pub status: String,
    pub attach: String,
    pub teardown: Vec<String>,
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

    /// Absolute path to the recipe's CLAUDE.md, if it declares one.
    pub fn claude_md_abs(&self) -> Option<PathBuf> {
        let rel = self.claude_md_path.as_ref()?;
        let dir = self.source.parent()?;
        Some(dir.join(rel))
    }

    /// Names of the lifecycle verbs this recipe overrides (empty ⇒ pure default).
    pub fn overrides(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.command.is_some() {
            v.push("command");
        }
        if self.workdir.is_some() {
            v.push("workdir");
        }
        if self.setup.is_some() {
            v.push("setup");
        }
        if self.launch.is_some() {
            v.push("launch");
        }
        if self.status.is_some() {
            v.push("status");
        }
        if self.attach.is_some() {
            v.push("attach");
        }
        if self.teardown.is_some() {
            v.push("teardown");
        }
        v
    }

    /// Resolve this recipe into a concrete [`Plan`] for one session: substitute
    /// every `{var}` and fill in defaults for any lifecycle verb left unset.
    pub fn plan(
        &self,
        uuid: &str,
        short: &str,
        sessions_root: &Path,
        repo: Option<&Path>,
    ) -> Result<Plan> {
        let session_name = format!("agent-{}", short);
        let recipe_dir = self
            .source
            .parent()
            .ok_or_else(|| anyhow!("recipe source has no parent: {}", self.source.display()))?;

        // Phase 1: base variables (everything except workdir/command, which may
        // themselves be templated from these).
        let mut base: HashMap<&str, String> = HashMap::new();
        base.insert("uuid", uuid.to_string());
        base.insert("short", short.to_string());
        base.insert("session", session_name.clone());
        base.insert("sessions_root", sessions_root.display().to_string());
        base.insert("recipe_dir", recipe_dir.display().to_string());
        if let Some(cm) = self.claude_md_abs() {
            base.insert("claude_md", cm.display().to_string());
        }
        if let Some(r) = repo {
            base.insert("repo", r.display().to_string());
        }

        let command = subst(self.command.as_deref().unwrap_or(DEFAULT_COMMAND), &base)?;
        let workdir = subst(self.workdir.as_deref().unwrap_or(DEFAULT_WORKDIR), &base)?;

        // Phase 2: full context, now including workdir + command.
        let mut vars = base;
        vars.insert("workdir", workdir.clone());
        vars.insert("command", command.clone());

        let setup = subst_list(self.setup.as_deref(), DEFAULT_SETUP, &vars)?;
        let launch = subst(self.launch.as_deref().unwrap_or(DEFAULT_LAUNCH), &vars)?;
        let status = subst(self.status.as_deref().unwrap_or(DEFAULT_STATUS), &vars)?;
        let attach = subst(self.attach.as_deref().unwrap_or(DEFAULT_ATTACH), &vars)?;
        let teardown = subst_list(self.teardown.as_deref(), DEFAULT_TEARDOWN, &vars)?;

        Ok(Plan {
            session_name,
            workdir: PathBuf::from(workdir),
            command,
            setup,
            launch,
            status,
            attach,
            teardown,
        })
    }
}

/// Substitute `{var}` placeholders from `vars`. Known variable names that are
/// referenced but unset are a hard error (with a hint); unknown `{...}` are left
/// untouched so shell constructs like `${HOME}` survive.
fn subst(tpl: &str, vars: &HashMap<&str, String>) -> Result<String> {
    let mut s = tpl.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{}}}", k), v);
    }
    for k in KNOWN_VARS {
        if !vars.contains_key(k) {
            let tok = format!("{{{}}}", k);
            if s.contains(&tok) {
                bail!("template references {} but it is not set{}", tok, hint(k));
            }
        }
    }
    Ok(s)
}

/// Resolve a list of steps: the recipe's own list if present, else the default.
fn subst_list(
    custom: Option<&[String]>,
    default: &[&str],
    vars: &HashMap<&str, String>,
) -> Result<Vec<String>> {
    match custom {
        Some(list) => list.iter().map(|s| subst(s, vars)).collect(),
        None => default.iter().map(|s| subst(s, vars)).collect(),
    }
}

fn hint(var: &str) -> &'static str {
    match var {
        "repo" => " (recipe has no `repository`; pass --repo)",
        "claude_md" => " (recipe has no `claude_md_path`)",
        _ => "",
    }
}

/// Resolve a recipe reference. If the arg looks like a path (contains a `/` or
/// ends with `.toml`), load directly. Otherwise treat as a name and look it up
/// in the search path.
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
