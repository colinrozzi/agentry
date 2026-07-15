//! Recipe parsing, resolution, and instantiation planning.
//!
//! A recipe is the instantiation document for an agent — its Dockerfile. It's a
//! TOML file describing an agent template: identity (name, description, a
//! CLAUDE.md guide) plus a `runtime` that decides how the agent runs.
//!
//! - `container` (default): agentry runs the agent in a docker/podman container
//!   with `~/.claude` and the working directory mounted in — see
//!   [`Recipe::container_steps`].
//! - `foreground`: runs `command` (default `claude`) in your terminal, tearing
//!   down on exit. Zero-dependency, not tracked.
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

/// Default agent image for the container runtime (built from the repo's
/// `Dockerfile`; see `agentry image build`).
const DEFAULT_IMAGE: &str = "agentry-agent:latest";

/// The command run inside the session.
const DEFAULT_COMMAND: &str = "claude";
/// Where the session's working directory lives.
const DEFAULT_WORKDIR: &str = "{sessions_root}/{uuid}";
/// Default workspace provisioning: just ensure the working directory exists.
/// agentry bakes in no jj/tmux — recipes declare any real provisioning.
const DEFAULT_SETUP: &[&str] = &["mkdir -p {workdir}"];
/// Default teardown: remove the working directory.
const DEFAULT_TEARDOWN: &[&str] = &["rm -rf {workdir}"];

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

/// How agentry runs a session's agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Built-in: run the agent inside a container (docker/podman), isolated,
    /// with `~/.claude` and the working directory mounted in. Default.
    #[default]
    Container,
    /// Run the agent in your terminal, tearing down on exit. Zero-dependency;
    /// not tracked by `list`/`attach`/`stop`.
    Foreground,
    /// Run the recipe's own declared `setup`/`launch`/`status`/`attach`/
    /// `teardown` steps — the escape hatch for tmux, cloud runners, anything.
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

    /// How to run the agent. Default `container`.
    #[serde(default)]
    pub runtime: Runtime,

    /// Command run inside the session. Default `claude`.
    #[serde(default)]
    pub command: Option<String>,
    /// The session's working directory (mounted into the container for the
    /// container runtime). Default `{sessions_root}/{uuid}`.
    #[serde(default)]
    pub workdir: Option<String>,

    // ---- Container runtime knobs ----
    /// Container image to run. Default `agentry-agent:latest`.
    #[serde(default)]
    pub image: Option<String>,
    /// Extra bind mounts (`host:container`), passed as `-v` flags. `~/.claude`
    /// and the working directory are always mounted.
    #[serde(default)]
    pub mounts: Option<Vec<String>>,
    /// Mount the agentry control socket into the container (at `/run/agentry.sock`)
    /// so the agent can manage the host fleet — `agentry list/start/stop/recipes`.
    /// A trust grant: the agent gains full control of your fleet. Container only.
    #[serde(default)]
    pub control_socket: bool,

    // ---- Shell runtime: declared lifecycle steps (unset ⇒ minimal default) ----
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

        let (setup, launch, status, attach, teardown) = match self.runtime {
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
            Runtime::Container => {
                let engine = container_engine().ok_or_else(|| {
                    anyhow!(
                        "no container engine found — install docker or podman, \
                         or set AGENTRY_CONTAINER_ENGINE (or use runtime = \"foreground\")"
                    )
                })?;
                self.container_steps(&engine, &session_name, &workdir, &command, &vars)?
            }
        };

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
        })
    }

    /// Build the built-in container lifecycle: a detached container with
    /// `~/.claude` and the working directory mounted, running the command under
    /// tmux (for interactive attach). Returns
    /// `(setup, launch, status, attach, teardown)`.
    fn container_steps(
        &self,
        engine: &str,
        session: &str,
        workdir: &str,
        command: &str,
        vars: &HashMap<&str, String>,
    ) -> Result<Steps> {
        let image = self.image.as_deref().unwrap_or(DEFAULT_IMAGE);
        let claude_home = claude_home()?;

        // Always mount the caller's claude auth + the working directory.
        let mut mount_flags = format!(
            "-v {}:/root/.claude -v {}:/work",
            claude_home.display(),
            workdir
        );
        if let Some(extra) = &self.mounts {
            for m in extra {
                mount_flags.push_str(&format!(" -v {}", subst(m, vars)?));
            }
        }
        // Mount the control socket so the agent can drive the host daemon.
        if self.control_socket {
            let sock = crate::protocol::socket_path();
            mount_flags.push_str(&format!(
                " -v {}:/run/agentry.sock -e AGENTRY_SOCKET=/run/agentry.sock",
                sock.display()
            ));
        }

        // Provision the host working directory (mounted at /work) and start a
        // long-lived container. Its PID 1 is `sleep infinity` so it stays up
        // independent of the agent; tmux + the agent are started via `exec`.
        let mut setup = vec![format!("mkdir -p {}", workdir)];
        if let Some(cm) = vars.get("claude_md") {
            setup.push(format!("cp {} {}/CLAUDE.md", cm, workdir));
        }
        setup.push(format!(
            "{engine} run -d --name {session} -e TERM=xterm-256color {mount_flags} -w /work \
             {image} sleep infinity"
        ));

        // Carry the caller's Claude config (~/.claude.json: onboarding state +
        // account) into the container so the agent doesn't re-run onboarding —
        // the mounted ~/.claude only holds the token, not this. Copied, not
        // mounted, so each container is isolated and never writes back to the
        // host file; best-effort (skipped if the caller has no ~/.claude.json).
        let claude_json = claude_json_host();
        setup.push(format!(
            "[ -f {json} ] && {engine} cp {json} {session}:/root/.claude.json || true",
            json = claude_json.display()
        ));
        // Trust the working directory so the agent skips the "trust this folder?"
        // prompt: patch ~/.claude.json in-container via the image's baked jq
        // program. A no-op if jq is absent (older image), so spawn never breaks.
        setup.push(format!(
            "{engine} exec {session} sh -c 'command -v jq >/dev/null 2>&1 || exit 0; \
             [ -f \"$HOME/.claude.json\" ] || printf \"{{}}\" > \"$HOME/.claude.json\"; \
             jq -f /etc/agentry-trust.jq \"$HOME/.claude.json\" > \"$HOME/.claude.json.tmp\" \
             && mv \"$HOME/.claude.json.tmp\" \"$HOME/.claude.json\"'"
        ));

        // Start the agent under tmux inside the container (attachable via exec).
        let launch = format!("{engine} exec -d {session} tmux new-session -d -s agent {command}");
        // Liveness = the agent's tmux session is alive (fails if the container
        // is gone or the agent has exited).
        let status = format!("{engine} exec {session} tmux has-session -t agent");
        let attach = format!("{engine} exec -it {session} tmux attach -t agent");
        let teardown = vec![format!("{engine} rm -f {session}")];

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
    fn runtime_defaults_to_container() {
        let r = recipe("name = \"x\"\nclaude_md_path = \"./C.md\"\n");
        assert_eq!(r.runtime, Runtime::Container);
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
    fn container_steps_shape() {
        let r = recipe("name = \"x\"\n");
        let workdir = "/s/u";
        let mut v = vars(&[("workdir", workdir), ("command", "claude")]);
        v.insert("claude_md", "/tmp/recipes/x/C.md".into());
        let (setup, launch, status, attach, teardown) = r
            .container_steps("docker", "agent-abcd1234", workdir, "claude", &v)
            .unwrap();

        assert_eq!(setup[0], "mkdir -p /s/u");
        assert!(setup
            .iter()
            .any(|s| s.contains("cp /tmp/recipes/x/C.md /s/u/CLAUDE.md")));
        let run = setup.iter().find(|s| s.contains("run -d")).unwrap();
        assert!(run.contains("docker run -d --name agent-abcd1234"));
        assert!(
            launch.contains("docker exec -d agent-abcd1234 tmux new-session -d -s agent claude")
        );
        assert!(status.contains("docker exec agent-abcd1234 tmux has-session -t agent"));
        assert!(attach.contains("docker exec -it agent-abcd1234 tmux attach -t agent"));
        assert_eq!(teardown, vec!["docker rm -f agent-abcd1234"]);
        // default image
        assert!(run.contains("agentry-agent:latest"));
        // caller's onboarding config is copied in, and /work is trusted
        assert!(setup.iter().any(|s| s.contains(":/root/.claude.json")));
        assert!(setup.iter().any(|s| s.contains("agentry-trust.jq")));
    }

    #[test]
    fn container_steps_honor_custom_image_and_mounts() {
        let r = recipe("name = \"x\"\nimage = \"my/img:1\"\nmounts = [\"/host:/c\"]\n");
        let v = vars(&[("workdir", "/w"), ("command", "claude")]);
        let (setup, _, _, _, _) = r
            .container_steps("podman", "agent-z", "/w", "claude", &v)
            .unwrap();
        // image + extra mounts land in the `run` command (the last setup step).
        let run = setup.iter().find(|s| s.contains("run -d")).unwrap();
        assert!(run.starts_with("podman run -d"));
        assert!(run.contains("my/img:1"));
        assert!(run.contains("-v /host:/c"));
        assert!(run.contains("-v /w:/work"));
    }

    #[test]
    fn container_steps_mount_control_socket() {
        let r = recipe("name = \"x\"\ncontrol_socket = true\n");
        let v = vars(&[("workdir", "/w"), ("command", "claude")]);
        let (setup, _, _, _, _) = r
            .container_steps("podman", "agent-z", "/w", "claude", &v)
            .unwrap();
        let run = setup.iter().find(|s| s.contains("run -d")).unwrap();
        assert!(run.contains(":/run/agentry.sock"), "run: {run}");
        assert!(
            run.contains("-e AGENTRY_SOCKET=/run/agentry.sock"),
            "run: {run}"
        );
    }
}
