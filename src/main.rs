//! agentry — a CLI tool for managing local AI agent sessions.

use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;
mod cmd;
mod daemon;
mod protocol;
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
    /// Run the agentry daemon (owns session state, serves the control socket).
    Daemon,

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
        /// Print the raw recipe.toml instead of formatted metadata.
        #[arg(long)]
        raw: bool,
    },
    /// Create or update a recipe from a directory (recipe.toml [+ CLAUDE.md]).
    Write {
        /// Recipe name (the directory it's written under).
        name: String,
        /// Directory containing recipe.toml (and optionally CLAUDE.md).
        #[arg(long)]
        from: String,
    },
    /// Delete a recipe by name.
    Rm {
        /// Recipe name.
        name: String,
    },
    /// Export a recipe to a shareable bundle (a `.recipe` tarball of its directory).
    Export {
        /// Recipe name.
        name: String,
        /// Output file (default: `<name>.recipe` in the current directory).
        #[arg(short, long)]
        out: Option<String>,
    },
    /// Install a recipe bundle (from `recipes export`) into your recipes directory.
    Install {
        /// Path to a `.recipe` bundle.
        path: String,
    },
    /// Import one recipe by name from a source: a git repo, a directory, or a `.recipe` URL.
    Import {
        /// Recipe name.
        name: String,
        /// Source: `owner/repo`, a git URL, a local directory, or a `.recipe` URL.
        source: String,
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
        // Local commands — no daemon needed.
        Cmd::Daemon => daemon::serve(),
        Cmd::Init { force } => cmd::init(force),
        Cmd::Image { cmd } => match cmd {
            ImageCmd::Build => cmd::image_build(),
        },
        // Everything stateful goes through the daemon via the client.
        Cmd::Recipes { cmd } => match cmd {
            RecipesCmd::List => client::recipes_list(),
            RecipesCmd::Show { recipe, raw } => client::recipes_show(&recipe, raw),
            RecipesCmd::Write { name, from } => {
                client::recipes_write(&name, std::path::Path::new(&from))
            }
            RecipesCmd::Rm { name } => client::recipes_delete(&name),
            // Export/install are local file operations — no daemon needed.
            RecipesCmd::Export { name, out } => cmd::recipes_export(&name, out.as_deref()),
            RecipesCmd::Install { path } => cmd::recipes_install(std::path::Path::new(&path)),
            RecipesCmd::Import { name, source } => cmd::recipes_import(&name, &source),
        },
        Cmd::Start {
            recipe,
            repo,
            r#for,
        } => client::start(&recipe, repo.as_deref(), r#for.as_deref()),
        Cmd::List => client::list(),
        Cmd::Stop { name } => client::stop(&name),
        Cmd::Show { name } => client::show(&name),
        Cmd::Attach { name } => client::attach(&name),
    }
}
