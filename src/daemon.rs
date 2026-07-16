//! The agentry daemon: owns session state, serves the control socket, and
//! executes lifecycle plans. Clients (the CLI, or a containerized agent with
//! the socket mounted) talk to it over the protocol in [`crate::protocol`].

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::protocol::{ForegroundPlan, Request, Response, SessionView, PROTOCOL_VERSION};
use crate::recipe::{self, Runtime};
use crate::session::{self, Session};

/// Run the daemon: bind the control socket and serve requests (one per
/// connection, serially — fine for one-human-one-machine use).
pub fn serve() -> Result<()> {
    let path = crate::protocol::socket_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // A stale socket from a previous run would block bind().
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    println!("agentry daemon listening on {}", path.display());

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle_conn(stream) {
                    eprintln!("connection error: {e}");
                }
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    Ok(())
}

fn handle_conn(mut stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let resp = match serde_json::from_str::<Request>(&line) {
        Ok(req) if req.v != PROTOCOL_VERSION => Response::err(format!(
            "protocol version mismatch: daemon speaks {PROTOCOL_VERSION}, client sent {}",
            req.v
        )),
        Ok(req) => dispatch(req),
        Err(e) => Response::err(format!("bad request: {e}")),
    };

    let mut out = serde_json::to_string(&resp)?;
    out.push('\n');
    stream.write_all(out.as_bytes())?;
    Ok(())
}

fn dispatch(req: Request) -> Response {
    match req.op.as_str() {
        "recipes.list" => respond(handle_recipes_list()),
        "recipes.show" => respond(handle_recipes_show(&req.args)),
        "recipes.write" => respond(handle_recipes_write(&req.args)),
        "recipes.delete" => respond(handle_recipes_delete(&req.args)),
        "session.list" => respond(handle_session_list()),
        "session.show" => respond(handle_session_show(&req.args)),
        "session.start" => respond(handle_session_start(&req.args)),
        "session.stop" => respond(handle_session_stop(&req.args)),
        "session.attach" => respond(handle_session_attach(&req.args)),
        other => Response::err(format!("unknown op: {other}")),
    }
}

fn respond(r: Result<Value>) -> Response {
    match r {
        Ok(v) => Response::ok(v),
        Err(e) => Response::err(e.to_string()),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing required argument '{key}'"))
}

// ---- handlers ----

fn handle_recipes_list() -> Result<Value> {
    let recipes = recipe::list_all()?;
    let list: Vec<Value> = recipes
        .iter()
        .map(|r| json!({ "name": r.name, "description": r.description }))
        .collect();
    Ok(json!({ "recipes": list }))
}

fn handle_recipes_show(args: &Value) -> Result<Value> {
    let r = recipe::resolve(arg_str(args, "recipe")?)?;
    // Raw file contents too, so a client (e.g. a container agent) can edit them.
    let recipe_toml = fs::read_to_string(&r.source).unwrap_or_default();
    let claude_md_content = r.claude_md_abs().and_then(|p| fs::read_to_string(p).ok());
    Ok(json!({
        "name": r.name,
        "description": r.description,
        "repository": r.repository,
        "source": r.source,
        "claude_md": r.claude_md_abs(),
        "runtime": r.runtime.as_str(),
        "overrides": r.overrides(),
        "recipe_toml": recipe_toml,
        "claude_md_content": claude_md_content,
    }))
}

/// Create or update a recipe under the primary search-path directory. The TOML
/// is validated (must parse as a recipe) before anything is written.
fn handle_recipes_write(args: &Value) -> Result<Value> {
    let name = arg_str(args, "name")?;
    if name.is_empty() || name.contains('/') {
        bail!("invalid recipe name '{name}'");
    }
    let recipe_toml = arg_str(args, "recipe_toml")?;
    toml::from_str::<recipe::Recipe>(recipe_toml)
        .map_err(|e| anyhow!("recipe does not parse: {e}"))?;

    let root = recipe::search_path()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("could not determine a recipes directory"))?;
    let dir = root.join(name);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("recipe.toml"), recipe_toml)?;
    if let Some(cm) = args.get("claude_md").and_then(|v| v.as_str()) {
        fs::write(dir.join("CLAUDE.md"), cm)?;
    }
    Ok(json!({ "name": name, "path": dir.join("recipe.toml") }))
}

/// Delete a named recipe (its directory) from the search path.
fn handle_recipes_delete(args: &Value) -> Result<Value> {
    let name = arg_str(args, "name")?;
    if name.is_empty() || name.contains('/') {
        bail!("invalid recipe name '{name}'");
    }
    let dir = recipe::search_path()
        .into_iter()
        .map(|root| root.join(name))
        .find(|d| d.join("recipe.toml").exists())
        .ok_or_else(|| anyhow!("recipe '{name}' not found"))?;
    fs::remove_dir_all(&dir)?;
    Ok(json!({ "name": name }))
}

fn handle_session_list() -> Result<Value> {
    let views: Vec<SessionView> = session::list_all()?
        .into_iter()
        .map(|s| {
            let status = status_of(&s);
            SessionView { session: s, status }
        })
        .collect();
    Ok(json!({ "sessions": serde_json::to_value(views)? }))
}

fn handle_session_show(args: &Value) -> Result<Value> {
    let s = session::find(arg_str(args, "name")?)?;
    let status = status_of(&s);
    Ok(serde_json::to_value(SessionView { session: s, status })?)
}

fn handle_session_stop(args: &Value) -> Result<Value> {
    let s = session::find(arg_str(args, "name")?)?;
    run_steps_best_effort(&s.teardown);
    s.delete()?;
    Ok(json!({ "name": s.name }))
}

fn handle_session_attach(args: &Value) -> Result<Value> {
    let s = session::find(arg_str(args, "name")?)?;
    if s.attach_cmd.trim().is_empty() {
        bail!("session '{}' has no attach command", s.name);
    }
    // The daemon has no tty — hand the command back for the client to exec.
    Ok(json!({ "name": s.name, "attach_cmd": s.attach_cmd }))
}

fn handle_session_start(args: &Value) -> Result<Value> {
    let recipe = recipe::resolve(arg_str(args, "recipe")?)?;
    let repo = args
        .get("repo")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| recipe.repository.clone());
    if let Some(r) = &repo {
        if !r.exists() {
            bail!("repository not found: {}", r.display());
        }
    }
    let ticket = args.get("for").and_then(|v| v.as_str());

    let uuid = uuid::Uuid::new_v4().to_string();
    let short = session::short_name(&uuid);
    let sessions_root = session::sessions_root()?;
    fs::create_dir_all(&sessions_root)?;
    let plan = recipe.plan(&uuid, &short, &sessions_root, repo.as_deref())?;

    // Setup runs in the daemon (host fs), with the AGENTRY_* context exported so
    // a launch script can read it. Roll back on failure.
    for step in &plan.setup {
        if !sh_env(step, &plan.env) {
            run_steps_best_effort(&plan.teardown);
            bail!("setup step failed: {}", step);
        }
    }

    // Foreground: the daemon can't host a tty — hand the plan to the client,
    // which runs the command in its terminal and tears down on exit. Not
    // registered (foreground sessions are ephemeral).
    if plan.runtime == Runtime::Foreground {
        let fp = ForegroundPlan {
            name: short,
            recipe_name: recipe.name.clone(),
            command: plan.command.clone(),
            workdir: plan.workdir.clone(),
            teardown: plan.teardown.clone(),
        };
        return Ok(json!({ "foreground": serde_json::to_value(fp)? }));
    }

    // Container / shell: background the runtime here, then register.
    if plan.launch.trim().is_empty() {
        run_steps_best_effort(&plan.teardown);
        bail!(
            "recipe '{}' uses runtime = \"shell\" but declares no `launch`",
            recipe.name
        );
    }
    if !sh_env(&plan.launch, &plan.env) {
        run_steps_best_effort(&plan.teardown);
        bail!("launch failed: {}", plan.launch);
    }

    let session = Session {
        uuid,
        name: short.clone(),
        recipe_name: recipe.name.clone(),
        recipe_path: recipe.source.clone(),
        repository: repo.clone(),
        workdir: plan.workdir.clone(),
        session_name: plan.session_name.clone(),
        command: plan.command.clone(),
        status_cmd: plan.status.clone(),
        attach_cmd: plan.attach.clone(),
        teardown: plan.teardown.clone(),
        started_at: session::now_rfc3339()?,
        linked_ticket: ticket.map(|s| s.to_string()),
    };
    session.save()?;

    Ok(json!({
        "name": short,
        "session": plan.session_name,
        "runtime": plan.runtime.as_str(),
        "repo": repo,
        "workdir": plan.workdir,
    }))
}

// ---- execution helpers (moved from the old cmd.rs) ----

/// Run a step through `sh -c` with `AGENTRY_*` context exported; returns whether
/// it succeeded. Used for `setup`/`launch`, which a `launch.sh` reads.
fn sh_env(cmd: &str, env: &[(String, String)]) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run steps best-effort (teardown / rollback): ignore individual failures.
pub fn run_steps_best_effort(steps: &[String]) {
    for step in steps {
        let _ = Command::new("sh").arg("-c").arg(step).status();
    }
}

/// "running" | "stale" | "unknown" for a session.
fn status_of(s: &Session) -> String {
    match check_running(&effective_status_cmd(s)) {
        Some(true) => "running",
        Some(false) => "stale",
        None => "unknown",
    }
    .to_string()
}

/// Legacy state files (pre-lifecycle-engine) carry no `status_cmd`; every such
/// session was a tmux session, so fall back to a tmux liveness check.
fn effective_status_cmd(s: &Session) -> String {
    if s.status_cmd.trim().is_empty() && !s.session_name.trim().is_empty() {
        format!("tmux has-session -t {}", s.session_name)
    } else {
        s.status_cmd.clone()
    }
}

fn check_running(status_cmd: &str) -> Option<bool> {
    if status_cmd.trim().is_empty() {
        return None;
    }
    match Command::new("sh")
        .arg("-c")
        .arg(status_cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) => Some(s.success()),
        Err(_) => Some(false),
    }
}
