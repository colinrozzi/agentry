//! CLI subcommand implementations.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::recipe;
use crate::session::{self, Session};

/// Spawn a new agent session.
pub fn start(reference: &str, repo_override: Option<&str>, ticket: Option<&str>) -> Result<()> {
    let recipe = recipe::resolve(reference)?;
    let repo = repo_override
        .map(PathBuf::from)
        .or_else(|| recipe.repository.clone())
        .ok_or_else(|| {
            anyhow!(
                "no repository specified — recipe '{}' has no default; pass --repo",
                recipe.name
            )
        })?;
    let claude_md_src = recipe.claude_md_abs()?;
    if !claude_md_src.exists() {
        return Err(anyhow!(
            "recipe's CLAUDE.md not found at {}",
            claude_md_src.display()
        ));
    }
    if !repo.exists() {
        return Err(anyhow!("repository not found: {}", repo.display()));
    }

    let uuid = uuid::Uuid::new_v4().to_string();
    let short = session::short_name(&uuid);
    let tmux_session = format!("agent-{}", short);
    let workspace_name = tmux_session.clone();
    let worktree_root = session::worktree_root()?;
    std::fs::create_dir_all(&worktree_root)
        .with_context(|| format!("creating worktree root {}", worktree_root.display()))?;
    let worktree = worktree_root.join(&uuid);

    // jj workspace add — sibling workspace on top of main, no branch pinning.
    // Repo must be jj-colocated (git worktrees pin a branch and would block a
    // second concurrent session on the same recipe).
    let status = Command::new("jj")
        .arg("-R")
        .arg(&repo)
        .arg("workspace")
        .arg("add")
        .arg("-r")
        .arg("main")
        .arg("--name")
        .arg(&workspace_name)
        .arg(&worktree)
        .status()
        .context("running jj workspace add")?;
    if !status.success() {
        return Err(anyhow!(
            "jj workspace add failed (exit {:?})",
            status.code()
        ));
    }

    // Copy the recipe's CLAUDE.md into the worktree root
    let claude_md_dst = worktree.join("CLAUDE.md");
    std::fs::copy(&claude_md_src, &claude_md_dst)
        .with_context(|| format!("copying CLAUDE.md to {}", claude_md_dst.display()))?;

    // Start a detached tmux session running `claude` in the worktree
    let status = Command::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&tmux_session)
        .arg("-c")
        .arg(&worktree)
        .arg("claude")
        .status()
        .context("running tmux new-session")?;
    if !status.success() {
        // Try to clean up the workspace if tmux didn't start
        let _ = Command::new("jj")
            .arg("-R")
            .arg(&repo)
            .args(["workspace", "forget"])
            .arg(&workspace_name)
            .status();
        let _ = std::fs::remove_dir_all(&worktree);
        return Err(anyhow!("tmux new-session failed (exit {:?})", status.code()));
    }

    let session = Session {
        uuid: uuid.clone(),
        name: short.clone(),
        recipe_name: recipe.name.clone(),
        recipe_path: recipe.source.clone(),
        repository: repo.clone(),
        worktree: worktree.clone(),
        tmux_session: tmux_session.clone(),
        started_at: session::now_rfc3339()?,
        linked_ticket: ticket.map(|s| s.to_string()),
    };
    session.save()?;

    println!("spawned {} (recipe={}, repo={})", short, recipe.name, repo.display());
    println!("  worktree: {}", worktree.display());
    println!("  tmux:     {}", tmux_session);
    if let Some(t) = ticket {
        println!("  ticket:   {}", t);
    }
    println!();
    println!("attach with:  agentry attach {}", short);
    println!("              tmux attach -t {}", tmux_session);
    Ok(())
}

/// List currently-running sessions.
pub fn list() -> Result<()> {
    let sessions = session::list_all()?;
    if sessions.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }
    let live: std::collections::HashSet<String> = tmux_alive_sessions()?.into_iter().collect();

    let name_w = sessions.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let recipe_w = sessions.iter().map(|s| s.recipe_name.len()).max().unwrap_or(6).max(6);
    println!(
        "{:<nw$}  {:<rw$}  status      ticket  started_at",
        "name",
        "recipe",
        nw = name_w,
        rw = recipe_w
    );
    println!(
        "{:<nw$}  {:<rw$}  ------      ------  ----------",
        "----",
        "------",
        nw = name_w,
        rw = recipe_w
    );
    for s in sessions {
        let status = if live.contains(&s.tmux_session) {
            "running"
        } else {
            "stale  "
        };
        let ticket = s.linked_ticket.as_deref().unwrap_or("-");
        println!(
            "{:<nw$}  {:<rw$}  {}     {:<6}  {}",
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
    let live = tmux_alive_sessions()?.contains(&s.tmux_session);
    println!("name:          {}", s.name);
    println!("uuid:          {}", s.uuid);
    println!("status:        {}", if live { "running" } else { "stale" });
    println!("recipe:        {}", s.recipe_name);
    println!("recipe_path:   {}", s.recipe_path.display());
    println!("repository:    {}", s.repository.display());
    println!("worktree:      {}", s.worktree.display());
    println!("tmux:          {}", s.tmux_session);
    println!("started_at:    {}", s.started_at);
    println!(
        "linked_ticket: {}",
        s.linked_ticket.as_deref().unwrap_or("-")
    );
    Ok(())
}

/// Stop a session: kill tmux, forget jj workspace, remove worktree dir, delete state file.
pub fn stop(name: &str) -> Result<()> {
    let s = session::find(name)?;
    // tmux kill-session — ignore failure (session may already be dead)
    let _ = Command::new("tmux")
        .args(["kill-session", "-t"])
        .arg(&s.tmux_session)
        .status();
    // jj workspace forget — best-effort (session may have been created with an
    // older agentry that used `git worktree add`, or already forgotten).
    let _ = Command::new("jj")
        .arg("-R")
        .arg(&s.repository)
        .args(["workspace", "forget"])
        .arg(&s.tmux_session)
        .status();
    // Best-effort: also try git worktree remove for legacy sessions
    let _ = Command::new("git")
        .arg("-C")
        .arg(&s.repository)
        .args(["worktree", "remove", "--force"])
        .arg(&s.worktree)
        .status();
    // Final sweep — make sure the directory is gone
    let _ = std::fs::remove_dir_all(&s.worktree);
    s.delete()?;
    println!("stopped {} (workspace removed, state file deleted)", s.name);
    Ok(())
}

/// Attach the current terminal to an agent's tmux session.
pub fn attach(name: &str) -> Result<()> {
    let s = session::find(name)?;
    // Exec tmux so the current terminal becomes the attached client.
    let err = Command::new("tmux")
        .args(["attach", "-t"])
        .arg(&s.tmux_session)
        .status()
        .context("running tmux attach")?;
    if !err.success() {
        return Err(anyhow!("tmux attach failed (exit {:?})", err.code()));
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
    let name_w = recipes.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
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
    println!("claude.md:   {}", recipe.claude_md_abs()?.display());
    Ok(())
}

/// Query tmux for currently-running session names. Returns empty if tmux
/// isn't available or has no sessions.
fn tmux_alive_sessions() -> Result<Vec<String>> {
    let out = Command::new("tmux")
        .args(["ls", "-F", "#{session_name}"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            Ok(stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|s| s.to_string())
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}
