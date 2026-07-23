//! Recipe parsing, resolution, and instantiation planning.
//!
//! A recipe is the instantiation document for an agent — its Dockerfile. It's a
//! TOML file describing an agent template: identity (name, description, a
//! CLAUDE.md guide) plus a `runtime` that decides how the agent runs.
//!
//! - `foreground` (default): runs `command` (default `claude`) in your terminal,
//!   tearing down on exit. Zero-dependency and zero-config; not tracked.
//! - `container`: agentry runs the recipe's `launch.sh` (podman run, mounts,
//!   credentials, start the agent as PID 1); `attach`/`status`/`stop` are generic
//!   podman ops on the container — see [`Recipe::podman_steps`].
//! - `shell`: the recipe declares its own lifecycle as shell steps (`setup`,
//!   `launch`, `status`, `attach`, `teardown`) — the escape hatch for tmux, jj
//!   workspaces, cloud runners, anything.
//!
//! Steps are templated with `{var}` placeholders and run through `sh -c`.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The command run inside the session.
const DEFAULT_COMMAND: &str = "claude";
/// Where the session's working directory lives.
const DEFAULT_WORKDIR: &str = "{sessions_root}/{uuid}";
/// Default workspace provisioning: just ensure the working directory exists.
/// agentry bakes in no jj/tmux — recipes declare any real provisioning.
const DEFAULT_SETUP: &[&str] = &["mkdir -p {workdir}"];
/// Default teardown: remove the working directory.
const DEFAULT_TEARDOWN: &[&str] = &["rm -rf {workdir}"];
/// Default bring-up for the `container` runtime: run the recipe's launch script.
/// Everything container-specific (podman run, mounts, credentials, the agent)
/// lives in that script; agentry only owns the generic verbs.
const DEFAULT_LAUNCH: &str = "sh {recipe_dir}/launch.sh";

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
    "claude_home",
    "claude_json",
    "control_socket",
    "image",
    "repo",
    "command",
];

/// How agentry runs a session's agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Run the agent in your terminal, tearing down on exit. Zero-dependency and
    /// zero-config — the default for a bare recipe.
    #[default]
    Foreground,
    /// Run the agent in a podman container. agentry runs the recipe's `launch.sh`
    /// (which does the podman run, mounts, credentials, and starts the agent as
    /// PID 1); `attach`/`status`/`stop` are generic podman ops on the container,
    /// which is named after the session. See [`Recipe::podman_steps`].
    Container,
    /// Run the recipe's own declared `setup`/`launch`/`status`/`attach`/
    /// `teardown` steps — the escape hatch for tmux, jj workspaces, anything.
    Shell,
}

impl Runtime {
    pub fn as_str(self) -> &'static str {
        match self {
            Runtime::Container => "container",
            Runtime::Foreground => "foreground",
            Runtime::Shell => "shell",
        }
    }
}

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

    /// How to run the agent. Default `foreground`.
    #[serde(default)]
    pub runtime: Runtime,

    /// Command run inside the session. Default `claude`.
    #[serde(default)]
    pub command: Option<String>,
    /// An opening message sent to the agent on the user's behalf at launch,
    /// passed as `claude`'s first prompt. The agent starts the conversation
    /// instead of the user attaching to a blank prompt. Templated like other
    /// fields. Optional.
    #[serde(default)]
    pub message: Option<String>,
    /// The session's working directory (mounted into the container for the
    /// container runtime). Default `{sessions_root}/{uuid}`.
    #[serde(default)]
    pub workdir: Option<String>,

    /// Container image tag the recipe runs. If set and the recipe directory ships
    /// a `Dockerfile`, agentry builds it on `start` when the image isn't present
    /// yet — so a shared recipe is self-contained (no separate build step).
    /// Exposed to `launch.sh` as `$AGENTRY_IMAGE` and the `{image}` variable.
    #[serde(default)]
    pub image: Option<String>,

    // ---- Lifecycle steps (shell runtime; container runtime uses `launch`) ----
    /// Steps to provision the workspace. Default: `mkdir -p {workdir}`.
    #[serde(default)]
    pub setup: Option<Vec<String>>,
    /// Step to background the runtime (required for the shell runtime).
    #[serde(default)]
    pub launch: Option<String>,
    /// Liveness check — exit 0 means running.
    #[serde(default)]
    pub status: Option<String>,
    /// Interactive attach command.
    #[serde(default)]
    pub attach: Option<String>,
    /// Steps to stop the runtime and deprovision. Default: `rm -rf {workdir}`.
    #[serde(default)]
    pub teardown: Option<Vec<String>>,

    /// Internal: the path the recipe was loaded from.
    #[serde(skip)]
    pub source: PathBuf,
}

/// A resolved lifecycle: `(setup, launch, status, attach, teardown)`.
type Steps = (Vec<String>, String, String, String, Vec<String>);

/// A fully-resolved instantiation plan: every template variable substituted,
/// ready to execute. Produced by [`Recipe::plan`]. `launch`/`status`/`attach`
/// are empty strings when the recipe doesn't declare them.
#[derive(Debug, Clone)]
pub struct Plan {
    pub session_name: String,
    pub workdir: PathBuf,
    pub command: String,
    pub runtime: Runtime,
    pub setup: Vec<String>,
    pub launch: String,
    pub status: String,
    pub attach: String,
    pub teardown: Vec<String>,
    /// `AGENTRY_*` variables exported when running `setup`/`launch` — the
    /// context a `launch.sh` reads (session name, workdir, credential paths,
    /// control socket, opening message, …).
    pub env: Vec<(String, String)>,
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
        // Host context a launch script needs — the caller's credential paths and
        // the control socket. Available both as `{vars}` and as `AGENTRY_*` env.
        let claude_home_s = claude_home()?.display().to_string();
        let claude_json_s = claude_json_host().display().to_string();
        let control_socket_s = crate::protocol::socket_path().display().to_string();
        base.insert("claude_home", claude_home_s.clone());
        base.insert("claude_json", claude_json_s.clone());
        base.insert("control_socket", control_socket_s.clone());
        if let Some(img) = &self.image {
            base.insert("image", img.clone());
        }

        let base_command = subst(self.command.as_deref().unwrap_or(DEFAULT_COMMAND), &base)?;
        let workdir = subst(self.workdir.as_deref().unwrap_or(DEFAULT_WORKDIR), &base)?;

        // The opening message, sent on the user's behalf. For foreground/shell
        // it's appended to the command as claude's first positional prompt
        // (shell-quoted to survive as one arg). For the container runtime it's
        // passed to launch.sh as `$AGENTRY_MESSAGE`, so the script delivers it.
        let message = match self.message.as_deref() {
            Some(m) if !m.is_empty() => Some(subst(m, &base)?),
            _ => None,
        };
        let command = match &message {
            Some(m) => format!("{} {}", base_command, shell_quote(m)),
            None => base_command.clone(),
        };

        // Phase 2: full context, now including workdir + command.
        let mut vars = base;
        vars.insert("workdir", workdir.clone());
        vars.insert("command", command.clone());

        let (mut setup, launch, status, attach, teardown) = match self.runtime {
            Runtime::Foreground | Runtime::Shell => {
                // The declarative engine: the recipe's steps, or minimal defaults.
                (
                    subst_list(self.setup.as_deref(), DEFAULT_SETUP, &vars)?,
                    subst_opt(self.launch.as_deref(), &vars)?,
                    subst_opt(self.status.as_deref(), &vars)?,
                    subst_opt(self.attach.as_deref(), &vars)?,
                    subst_list(self.teardown.as_deref(), DEFAULT_TEARDOWN, &vars)?,
                )
            }
            Runtime::Container => self.podman_steps(&session_name, &vars)?,
        };

        // Self-contained recipes: if the recipe declares an image and ships a
        // Dockerfile next to it, build the image on `start` when it's missing —
        // so an installed recipe just works without a separate build step.
        if let Some(img) = &self.image {
            if self.runtime == Runtime::Container && recipe_dir.join("Dockerfile").is_file() {
                setup.insert(
                    0,
                    format!(
                        "podman image exists {img} || podman build {dir} -t {img}",
                        dir = recipe_dir.display()
                    ),
                );
            }
        }

        // The AGENTRY_* environment handed to setup/launch (what launch.sh reads).
        let mut env: Vec<(String, String)> = vec![
            ("AGENTRY_SESSION".into(), session_name.clone()),
            ("AGENTRY_WORKDIR".into(), workdir.clone()),
            (
                "AGENTRY_SESSIONS_ROOT".into(),
                sessions_root.display().to_string(),
            ),
            (
                "AGENTRY_RECIPE_DIR".into(),
                recipe_dir.display().to_string(),
            ),
            ("AGENTRY_CLAUDE_HOME".into(), claude_home_s),
            ("AGENTRY_CLAUDE_JSON".into(), claude_json_s),
            ("AGENTRY_CONTROL_SOCKET".into(), control_socket_s),
            ("AGENTRY_COMMAND".into(), base_command),
        ];
        if let Some(cm) = self.claude_md_abs() {
            env.push(("AGENTRY_CLAUDE_MD".into(), cm.display().to_string()));
        }
        if let Some(m) = &message {
            env.push(("AGENTRY_MESSAGE".into(), m.clone()));
        }
        if let Some(r) = repo {
            env.push(("AGENTRY_REPO".into(), r.display().to_string()));
        }
        if let Some(img) = &self.image {
            env.push(("AGENTRY_IMAGE".into(), img.clone()));
        }

        Ok(Plan {
            session_name,
            workdir: PathBuf::from(workdir),
            command,
            runtime: self.runtime,
            setup,
            launch,
            status,
            attach,
            teardown,
            env,
        })
    }

    /// Build the container lifecycle. The bring-up — podman run, mounts,
    /// credentials, starting the agent — is the recipe's `launch.sh` (run on the
    /// host with `AGENTRY_*` env). agentry owns only the generic verbs, keyed on
    /// the container name (which `launch.sh` sets to `$AGENTRY_SESSION`): the
    /// agent runs as the container's PID 1, so container-alive == agent-alive.
    /// Returns `(setup, launch, status, attach, teardown)`.
    fn podman_steps(&self, session: &str, vars: &HashMap<&str, String>) -> Result<Steps> {
        // setup is the recipe's (usually empty — launch.sh provisions the workdir).
        let setup = subst_list(self.setup.as_deref(), &[], vars)?;
        let launch = subst(self.launch.as_deref().unwrap_or(DEFAULT_LAUNCH), vars)?;
        // Liveness: is a container of this name running?
        let status = format!("podman ps -q -f name=^{session}$ -f status=running | grep -q .");
        // Attach straight to PID 1 (the agent). Detach with the podman sequence.
        let attach = format!("podman attach {session}");
        let teardown = vec![format!("podman rm -f {session}")];
        Ok((setup, launch, status, attach, teardown))
    }
}

/// The container engine to use: `$AGENTRY_CONTAINER_ENGINE`, else the first of
/// `docker`/`podman` found on PATH.
pub fn container_engine() -> Option<String> {
    if let Some(e) = std::env::var_os("AGENTRY_CONTAINER_ENGINE") {
        let e = e.to_string_lossy().into_owned();
        return if e.is_empty() { None } else { Some(e) };
    }
    ["docker", "podman"]
        .into_iter()
        .find(|e| on_path(e))
        .map(|e| e.to_string())
}

/// The caller's `~/.claude` directory (bind-mounted for container auth).
fn claude_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// The caller's `~/.claude.json` (Claude Code's user config: onboarding state +
/// account). Copied into the container so the agent inherits completed
/// onboarding. Infallible: if `HOME` is unset the path simply won't exist and
/// the copy step (guarded by `[ -f … ]`) is skipped.
fn claude_json_host() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude.json")
}

/// POSIX single-quote a string so it survives `sh -c` parsing as one argument.
/// Wraps in `'…'` and rewrites any embedded `'` as `'\''`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Whether a command is resolvable on PATH.
pub fn on_path(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", cmd))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

/// Resolve an optional single template, or the empty string if unset.
fn subst_opt(tpl: Option<&str>, vars: &HashMap<&str, String>) -> Result<String> {
    match tpl {
        Some(t) => subst(t, vars),
        None => Ok(String::new()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a recipe from TOML and give it a source with a parent directory,
    /// so `plan()`/`claude_md_abs()` can resolve relative paths.
    fn recipe(toml: &str) -> Recipe {
        let mut r: Recipe = toml::from_str(toml).expect("recipe parses");
        r.source = PathBuf::from("/tmp/recipes/x/recipe.toml");
        r
    }

    fn vars(pairs: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    #[test]
    fn subst_replaces_known_vars() {
        let v = vars(&[("workdir", "/w"), ("session", "agent-ab")]);
        assert_eq!(
            subst("{workdir}/x -s {session}", &v).unwrap(),
            "/w/x -s agent-ab"
        );
    }

    #[test]
    fn subst_leaves_unknown_braces_alone() {
        // Shell constructs like ${HOME} and awk '{print}' must survive.
        let v = vars(&[]);
        assert_eq!(subst("echo ${HOME}", &v).unwrap(), "echo ${HOME}");
        assert_eq!(subst("awk '{print}'", &v).unwrap(), "awk '{print}'");
    }

    #[test]
    fn subst_errors_on_known_but_unset_var() {
        // {repo} is a known variable; referenced but unset ⇒ hard error.
        let err = subst("jj -R {repo} ...", &vars(&[]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("{repo}"), "got: {err}");
    }

    #[test]
    fn runtime_defaults_to_foreground() {
        let r = recipe("name = \"x\"\nclaude_md_path = \"./C.md\"\n");
        assert_eq!(r.runtime, Runtime::Foreground);
    }

    #[test]
    fn foreground_plan_uses_defaults_and_no_launch() {
        let r = recipe("name = \"x\"\nruntime = \"foreground\"\n");
        let plan = r
            .plan("uuid-1", "abcd1234", Path::new("/sessions"), None)
            .unwrap();
        assert_eq!(plan.runtime, Runtime::Foreground);
        assert_eq!(plan.command, "claude");
        assert_eq!(plan.workdir, PathBuf::from("/sessions/uuid-1"));
        assert_eq!(plan.setup, vec!["mkdir -p /sessions/uuid-1"]);
        assert_eq!(plan.teardown, vec!["rm -rf /sessions/uuid-1"]);
        assert!(plan.launch.is_empty());
    }

    #[test]
    fn shell_plan_substitutes_declared_steps() {
        let r = recipe(
            "name = \"x\"\nruntime = \"shell\"\n\
             launch = \"tmux new-session -d -s {session} -c {workdir} {command}\"\n\
             status = \"tmux has-session -t {session}\"\n",
        );
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        assert_eq!(
            plan.launch,
            "tmux new-session -d -s agent-abcd1234 -c /s/u claude"
        );
        assert_eq!(plan.status, "tmux has-session -t agent-abcd1234");
    }

    #[test]
    fn message_is_appended_as_quoted_first_prompt() {
        // Foreground so we can read the resolved command directly.
        let r = recipe("name = \"x\"\nruntime = \"foreground\"\nmessage = \"hi there\"\n");
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        assert_eq!(plan.command, "claude 'hi there'");
    }

    #[test]
    fn message_with_apostrophe_is_escaped() {
        let r = recipe("name = \"x\"\nruntime = \"foreground\"\nmessage = \"don't panic\"\n");
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        // Single argument preserved: '…' with the embedded ' as '\''.
        assert_eq!(plan.command, "claude 'don'\\''t panic'");
    }

    #[test]
    fn container_plan_runs_launch_script_with_generic_verbs() {
        let r = recipe("name = \"x\"\nruntime = \"container\"\n");
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        // launch defaults to the recipe's launch.sh (recipe_dir = source parent).
        assert_eq!(plan.launch, "sh /tmp/recipes/x/launch.sh");
        // the verbs are generic podman ops keyed on the container name.
        assert_eq!(
            plan.status,
            "podman ps -q -f name=^agent-abcd1234$ -f status=running | grep -q ."
        );
        assert_eq!(plan.attach, "podman attach agent-abcd1234");
        assert_eq!(plan.teardown, vec!["podman rm -f agent-abcd1234"]);
        // no baked setup — launch.sh provisions everything.
        assert!(plan.setup.is_empty());
    }

    #[test]
    fn container_plan_honors_custom_launch() {
        let r =
            recipe("name = \"x\"\nruntime = \"container\"\nlaunch = \"sh {recipe_dir}/go.sh\"\n");
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        assert_eq!(plan.launch, "sh /tmp/recipes/x/go.sh");
    }

    #[test]
    fn plan_env_carries_context_and_raw_message() {
        let r = recipe(
            "name = \"x\"\nruntime = \"container\"\nclaude_md_path = \"./C.md\"\n\
             message = \"hi there\"\n",
        );
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        let env: std::collections::HashMap<_, _> = plan.env.into_iter().collect();
        assert_eq!(env.get("AGENTRY_SESSION").unwrap(), "agent-abcd1234");
        assert_eq!(env.get("AGENTRY_WORKDIR").unwrap(), "/s/u");
        // the message reaches launch.sh raw (not shell-quoted — the script quotes it).
        assert_eq!(env.get("AGENTRY_MESSAGE").unwrap(), "hi there");
        assert!(env.contains_key("AGENTRY_CLAUDE_HOME"));
        assert!(env.contains_key("AGENTRY_CONTROL_SOCKET"));
    }

    #[test]
    fn container_image_exposes_agentry_image() {
        let r = recipe("name = \"x\"\nruntime = \"container\"\nimage = \"my/img:1\"\n");
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        let env: std::collections::HashMap<_, _> = plan.env.into_iter().collect();
        assert_eq!(env.get("AGENTRY_IMAGE").unwrap(), "my/img:1");
        // no Dockerfile next to this synthetic recipe → no build step is inserted.
        assert!(!plan.setup.iter().any(|s| s.contains("podman build")));
    }

    #[test]
    fn container_image_with_dockerfile_builds_on_start() {
        let dir =
            std::env::temp_dir().join(format!("agentry-test-imgbuild-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM debian:stable-slim\n").unwrap();
        let mut r: Recipe =
            toml::from_str("name = \"x\"\nruntime = \"container\"\nimage = \"my/img:1\"\n")
                .unwrap();
        r.source = dir.join("recipe.toml");
        let plan = r.plan("u", "abcd1234", Path::new("/s"), None).unwrap();
        // the build-if-missing step is prepended to setup.
        assert!(
            plan.setup[0].contains("podman image exists my/img:1"),
            "{}",
            plan.setup[0]
        );
        assert!(plan.setup[0].contains(&format!("podman build {}", dir.display())));
        std::fs::remove_dir_all(&dir).ok();
    }
}
