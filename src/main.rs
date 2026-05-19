//! agentry — a CLI tool for managing local AI agent sessions.
//!
//! See README for the model. v0 implements recipe parsing + listing; spawn,
//! stop, list, attach are stubbed pending the worktree + tmux wiring.

use anyhow::Result;
use clap::{Parser, Subcommand};

mod recipe;

#[derive(Parser)]
#[command(name = "agentry", about, version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage recipes (agent identity templates).
    Recipes {
        #[command(subcommand)]
        cmd: RecipesCmd,
    },

    /// Spawn an agent session from a recipe.
    Start {
        /// Recipe name (looked up in the search path) or path to a recipe.toml.
        recipe: String,

        /// Override the recipe's default repository.
        #[arg(long)]
        repo: Option<String>,

        /// Optional ticket id this session is linked to.
        #[arg(long)]
        r#for: Option<String>,
    },

    /// List currently-running agent sessions.
    List,

    /// Stop a running agent session.
    Stop {
        /// Session name or UUID.
        name: String,
    },

    /// Show full state for a running agent session.
    Show {
        /// Session name or UUID.
        name: String,
    },

    /// Attach to an agent's tmux session.
    Attach {
        /// Session name or UUID.
        name: String,
    },
}

#[derive(Subcommand)]
enum RecipesCmd {
    /// List recipes found in the search path.
    List,
    /// Show one recipe's contents.
    Show {
        /// Recipe name or path to recipe.toml.
        recipe: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Recipes { cmd } => match cmd {
            RecipesCmd::List => recipes_list(),
            RecipesCmd::Show { recipe } => recipes_show(&recipe),
        },
        Cmd::Start { recipe, repo, r#for } => start_stub(&recipe, repo.as_deref(), r#for.as_deref()),
        Cmd::List => not_yet("list"),
        Cmd::Stop { name } => not_yet(&format!("stop {}", name)),
        Cmd::Show { name } => not_yet(&format!("show {}", name)),
        Cmd::Attach { name } => not_yet(&format!("attach {}", name)),
    }
}

fn recipes_list() -> Result<()> {
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

fn recipes_show(reference: &str) -> Result<()> {
    let recipe = recipe::resolve(reference)?;
    println!("name:        {}", recipe.name);
    println!("description: {}", recipe.description);
    println!("repository:  {}", recipe.repository.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none — must be supplied at spawn)".to_string()));
    println!("source:      {}", recipe.source.display());
    let claude_md_path = recipe.claude_md_abs()?;
    println!("claude.md:   {}", claude_md_path.display());
    Ok(())
}

fn start_stub(reference: &str, repo: Option<&str>, ticket: Option<&str>) -> Result<()> {
    let recipe = recipe::resolve(reference)?;
    let repo_path = repo
        .map(std::path::PathBuf::from)
        .or_else(|| recipe.repository.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no repository specified — recipe '{}' has no default; pass --repo",
                recipe.name
            )
        })?;
    println!("[would spawn] recipe: {}", recipe.name);
    println!("[would spawn] repository: {}", repo_path.display());
    if let Some(t) = ticket {
        println!("[would spawn] linked ticket: {}", t);
    }
    println!("[would spawn] worktree: ~/work/agentry-sessions/<new-uuid>/");
    println!("[would spawn] CLAUDE.md: copy from {}", recipe.claude_md_abs()?.display());
    println!("[would spawn] tmux session: agent-<uuid>");
    println!();
    println!("(spawn not yet wired up — coming next)");
    Ok(())
}

fn not_yet(what: &str) -> Result<()> {
    println!("'{}' is not yet implemented", what);
    Ok(())
}
