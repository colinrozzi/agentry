//! agentry — a CLI tool for managing local AI agent sessions.

use anyhow::Result;
use clap::{Parser, Subcommand};

mod cmd;
mod recipe;
mod session;

#[derive(Parser)]
#[command(name = "agentry", about, version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Seed the recipes directory with a starter onboarding-agent recipe.
    Init {
        /// Overwrite the onboarding-agent recipe if it already exists.
        #[arg(long)]
        force: bool,
    },

    /// Manage the agent container image.
    Image {
        #[command(subcommand)]
        cmd: ImageCmd,
    },

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
enum ImageCmd {
    /// Build the default agent image (agentry-agent:latest) from the bundled Dockerfile.
    Build,
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
    // Behave like a normal Unix tool when stdout is closed early (e.g.
    // `agentry list | head`): die on SIGPIPE instead of panicking. Rust sets
    // SIGPIPE to SIG_IGN by default, which turns broken pipes into panics.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { force } => cmd::init(force),
        Cmd::Image { cmd } => match cmd {
            ImageCmd::Build => cmd::image_build(),
        },
        Cmd::Recipes { cmd } => match cmd {
            RecipesCmd::List => cmd::recipes_list(),
            RecipesCmd::Show { recipe } => cmd::recipes_show(&recipe),
        },
        Cmd::Start {
            recipe,
            repo,
            r#for,
        } => cmd::start(&recipe, repo.as_deref(), r#for.as_deref()),
        Cmd::List => cmd::list(),
        Cmd::Stop { name } => cmd::stop(&name),
        Cmd::Show { name } => cmd::show(&name),
        Cmd::Attach { name } => cmd::attach(&name),
    }
}
