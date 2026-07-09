//! CLI subcommand implementations.
//!
//! The lifecycle commands are a thin orchestrator over the recipe's declared
//! steps: `start` runs `setup` then `launch`; `stop` runs the stored
//! `teardown`; `list`/`show` run each session's `status`; `attach` runs its
//! `attach`. Nothing here knows about jj or tmux — those live only in the
//! recipe's default template (see `recipe.rs`).

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::recipe;
use crate::session::{self, Session};

/// The starter recipe seeded by `agentry init`, embedded at build time.
const ONBOARDING_RECIPE_TOML: &str = include_str!("assets/onboarding-agent.recipe.toml");
const ONBOARDING_CLAUDE_MD: &str = include_str!("assets/onboarding-agent.CLAUDE.md");
/// The default agent image's Dockerfile, embedded at build time.
const AGENT_DOCKERFILE: &str = include_str!("assets/agent.Dockerfile");

/// Build the default agent image (`agentry-agent:latest`) from the bundled
/// Dockerfile using the detected container engine.
pub fn image_build() -> Result<()> {
    let engine = recipe::container_engine().ok_or_else(|| {
        anyhow!("no container engine found — install docker or podman, or set AGENTRY_CONTAINER_ENGINE")
    })?;
    let dir = std::env::temp_dir().join(format!("agentry-image-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating build context {}", dir.display()))?;
    std::fs::write(dir.join("Dockerfile"), AGENT_DOCKERFILE)
        .with_context(|| format!("writing Dockerfile to {}", dir.display()))?;

    println!("building agentry-agent:latest with {} …", engine);
    let ok = Command::new(&engine)
        .args(["build", "-t", "agentry-agent:latest"])
        .arg(&dir)
        .status()
        .with_context(|| format!("running {} build", engine));
    let _ = std::fs::remove_dir_all(&dir);
    if !ok?.success() {
        return Err(anyhow!("image build failed"));
    }
    println!("built agentry-agent:latest");
    Ok(())
}

/// Seed the recipes directory with the onboarding-agent recipe, so a fresh
/// install has something to spawn and a worked example to learn the format from.
pub fn init(force: bool) -> Result<()> {
    let recipes_root = recipe::search_path()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("could not determine a recipes directory"))?;
    let dir = recipes_root.join("onboarding-agent");
    let recipe_toml = dir.join("recipe.toml");
    let claude_md = dir.join("CLAUDE.md");

    if recipe_toml.exists() && !force {
        println!(
            "onboarding-agent recipe already exists at {}",
            recipe_toml.display()
        );
        println!("(pass --force to overwrite)");
    } else {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating recipe dir {}", dir.display()))?;
        std::fs::write(&recipe_toml, ONBOARDING_RECIPE_TOML)
            .with_context(|| format!("writing {}", recipe_toml.display()))?;
        std::fs::write(&claude_md, ONBOARDING_CLAUDE_MD)
            .with_context(|| format!("writing {}", claude_md.display()))?;
        println!("seeded onboarding-agent recipe at {}", dir.display());
        println!("  recipe: {}", recipe_toml.display());
        println!("  guide:  {}", claude_md.display());
    }
    println!();
    check_container_prereqs();
    print_next_steps();
    Ok(())
}

/// Agents run in containers by default, so warn early (at `init`) if the
/// container engine or the agent image isn't ready.
fn check_container_prereqs() {
    let engine = match recipe::container_engine() {
        Some(e) => e,
        None => {
            println!("⚠  No container engine found (docker/podman).");
            println!("   agentry runs agents in containers by default — install docker or");
            println!("   podman, or set AGENTRY_CONTAINER_ENGINE (or use runtime = \"foreground\").");
            println!();
            return;
        }
    };
    let image_present = Command::new(&engine)
        .args(["image", "inspect", "agentry-agent:latest"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !image_present {
        println!("⚠  The agent image `agentry-agent:latest` isn't built yet.");
        println!("   Build it once (installs claude inside the image):");
        println!("     agentry image build");
        println!();
    }
}

fn print_next_steps() {
    println!("next steps:");
    println!("  agentry image build              # build the agent image (once, if needed)");
    println!("  agentry start onboarding-agent   # spawn it in a container");
    println!("  agentry attach <name>            # attach and chat (name from `agentry list`)");
}

/// Spawn a new agent session.
pub fn start(reference: &str, repo_override: Option<&str>, ticket: Option<&str>) -> Result<()> {
    let recipe = recipe::resolve(reference)?;
    let repo = repo_override
        .map(PathBuf::from)
        .or_else(|| recipe.repository.clone());
    if let Some(r) = &repo {
        if !r.exists() {
            return Err(anyhow!("repository not found: {}", r.display()));
        }
    }

    let uuid = uuid::Uuid::new_v4().to_string();
    let short = session::short_name(&uuid);
    let sessions_root = session::sessions_root()?;
    std::fs::create_dir_all(&sessions_root)
        .with_context(|| format!("creating sessions root {}", sessions_root.display()))?;

    let plan = recipe.plan(&uuid, &short, &sessions_root, repo.as_deref())?;

    // Run setup steps in order. On any failure, roll back with teardown.
    for step in &plan.setup {
        if !sh(step)? {
            run_steps_best_effort(&plan.teardown);
            return Err(anyhow!("setup step failed: {}", step));
        }
    }

    if plan.runtime == recipe::Runtime::Foreground {
        // Foreground: run the command in this terminal, tear down on exit.
        // No state file — the session lives only as long as the process.
        println!(
            "starting {} (recipe={}) in {}",
            short,
            recipe.name,
            plan.workdir.display()
        );
        println!("(foreground session — it ends when you exit the agent)\n");
        let ran = Command::new("sh")
            .arg("-c")
            .arg(&plan.command)
            .current_dir(&plan.workdir)
            .status()
            .with_context(|| format!("running command: {}", plan.command));
        run_steps_best_effort(&plan.teardown);
        let ran = ran?;
        if !ran.success() {
            return Err(anyhow!("agent exited with status {:?}", ran.code()));
        }
        return Ok(());
    }

    // Detached (container or shell): background the runtime, then track it.
    // The container runtime always has a launch; a shell recipe must declare one.
    if plan.launch.trim().is_empty() {
        run_steps_best_effort(&plan.teardown);
        return Err(anyhow!(
            "recipe '{}' uses runtime = \"shell\" but declares no `launch`; \
             declare how to background the runtime (e.g. a tmux new-session)",
            recipe.name
        ));
    }
    if !sh(&plan.launch)? {
        run_steps_best_effort(&plan.teardown);
        return Err(anyhow!("launch failed: {}", plan.launch));
    }

    let session = Session {
        uuid: uuid.clone(),
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

    println!(
        "spawned {} (recipe={}, {})",
        short,
        recipe.name,
        plan.runtime.as_str()
    );
    if let Some(r) = &repo {
        println!("  repo:     {}", r.display());
    }
    println!("  workdir:  {}", plan.workdir.display());
    println!("  session:  {}", plan.session_name);
    if let Some(t) = ticket {
        println!("  ticket:   {}", t);
    }
    if !plan.attach.trim().is_empty() {
        println!("\nattach with:  agentry attach {}", short);
    }
    Ok(())
}

/// List currently-tracked sessions.
pub fn list() -> Result<()> {
    let sessions = session::list_all()?;
    if sessions.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }

    let name_w = sessions
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let recipe_w = sessions
        .iter()
        .map(|s| s.recipe_name.len())
        .max()
        .unwrap_or(6)
        .max(6);
    println!(
        "{:<nw$}  {:<rw$}  {:<8}  {:<6}  started_at",
        "name",
        "recipe",
        "status",
        "ticket",
        nw = name_w,
        rw = recipe_w
    );
    println!(
        "{:<nw$}  {:<rw$}  {:<8}  {:<6}  ----------",
        "----",
        "------",
        "------",
        "------",
        nw = name_w,
        rw = recipe_w
    );
    for s in sessions {
        let status = match check_running(&effective_status_cmd(&s)) {
            Some(true) => "running",
            Some(false) => "stale",
            None => "unknown",
        };
        let ticket = s.linked_ticket.as_deref().unwrap_or("-");
        println!(
            "{:<nw$}  {:<rw$}  {:<8}  {:<6}  {}",
            s.name,
            s.recipe_name,
            status,
            ticket,
            s.started_at,
            nw = name_w,
            rw = recipe_w
        );
    }
    Ok(())
}

/// Show full state for one session.
pub fn show(name: &str) -> Result<()> {
    let s = session::find(name)?;
    let status = match check_running(&effective_status_cmd(&s)) {
        Some(true) => "running",
        Some(false) => "stale",
        None => "unknown",
    };
    println!("name:          {}", s.name);
    println!("uuid:          {}", s.uuid);
    println!("status:        {}", status);
    println!("recipe:        {}", s.recipe_name);
    println!("recipe_path:   {}", s.recipe_path.display());
    println!(
        "repository:    {}",
        s.repository
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("workdir:       {}", s.workdir.display());
    println!("session:       {}", s.session_name);
    println!("command:       {}", s.command);
    println!("started_at:    {}", s.started_at);
    println!(
        "linked_ticket: {}",
        s.linked_ticket.as_deref().unwrap_or("-")
    );
    Ok(())
}

/// Stop a session: run its stored teardown, then delete the state file.
pub fn stop(name: &str) -> Result<()> {
    let s = session::find(name)?;
    run_steps_best_effort(&s.teardown);
    s.delete()?;
    println!("stopped {} (teardown complete, state file deleted)", s.name);
    Ok(())
}

/// Attach the current terminal to a session via its declared attach command.
pub fn attach(name: &str) -> Result<()> {
    let s = session::find(name)?;
    if s.attach_cmd.trim().is_empty() {
        return Err(anyhow!("session '{}' has no attach command", s.name));
    }
    let status = Command::new("sh")
        .arg("-c")
        .arg(&s.attach_cmd)
        .status()
        .with_context(|| format!("running attach command: {}", s.attach_cmd))?;
    if !status.success() {
        return Err(anyhow!("attach failed (exit {:?})", status.code()));
    }
    Ok(())
}

/// `agentry recipes list`
pub fn recipes_list() -> Result<()> {
    let recipes = recipe::list_all()?;
    if recipes.is_empty() {
        println!("(no recipes found in search path)");
        for p in recipe::search_path() {
            println!("  searched: {}", p.display());
        }
        return Ok(());
    }
    let name_w = recipes
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{:<width$}  description", "name", width = name_w);
    println!("{:<width$}  -----------", "----", width = name_w);
    for r in recipes {
        println!("{:<width$}  {}", r.name, r.description, width = name_w);
    }
    Ok(())
}

/// `agentry recipes show <name|path>`
pub fn recipes_show(reference: &str) -> Result<()> {
    let recipe = recipe::resolve(reference)?;
    println!("name:        {}", recipe.name);
    println!("description: {}", recipe.description);
    println!(
        "repository:  {}",
        recipe
            .repository
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none — must be supplied at spawn)".to_string())
    );
    println!("source:      {}", recipe.source.display());
    println!(
        "claude.md:   {}",
        recipe
            .claude_md_abs()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_string())
    );
    println!("runtime:     {}", recipe.runtime.as_str());
    if recipe.runtime == recipe::Runtime::Container {
        println!(
            "image:       {}",
            recipe.image.as_deref().unwrap_or("agentry-agent:latest (default)")
        );
    } else {
        let overrides = recipe.overrides();
        println!(
            "lifecycle:   {}",
            if overrides.is_empty() {
                "default".to_string()
            } else {
                format!("custom: {}", overrides.join(", "))
            }
        );
    }
    Ok(())
}

/// Run a single step through `sh -c`, returning whether it succeeded.
fn sh(cmd: &str) -> Result<bool> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .with_context(|| format!("running: {}", cmd))?;
    Ok(status.success())
}

/// Run steps through `sh -c`, ignoring individual failures (used for teardown
/// and rollback — each step should be independently best-effort).
fn run_steps_best_effort(steps: &[String]) {
    for step in steps {
        let _ = Command::new("sh").arg("-c").arg(step).status();
    }
}

/// The status command to actually run for a session. Legacy state files (from
/// before the lifecycle engine) carry no `status_cmd`; every such session was a
/// tmux session, so fall back to a tmux liveness check keyed on its name.
fn effective_status_cmd(s: &Session) -> String {
    if s.status_cmd.trim().is_empty() && !s.session_name.trim().is_empty() {
        format!("tmux has-session -t {}", s.session_name)
    } else {
        s.status_cmd.clone()
    }
}

/// Run a session's status command. `Some(true)` = running, `Some(false)` =
/// stale, `None` = no status command (unknown). Output is suppressed.
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
