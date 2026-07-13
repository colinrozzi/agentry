//! Local (non-daemon) CLI commands: `init` and `image build`. Everything
//! stateful (start/stop/list/show/attach, recipes) goes through the daemon —
//! see `client.rs` (client side) and `daemon.rs` (server side).

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};

use crate::recipe;

/// The starter recipe seeded by `agentry init`, embedded at build time.
const ONBOARDING_RECIPE_TOML: &str = include_str!("assets/onboarding-agent.recipe.toml");
const ONBOARDING_CLAUDE_MD: &str = include_str!("assets/onboarding-agent.CLAUDE.md");
/// The default agent image's Dockerfile, embedded at build time.
const AGENT_DOCKERFILE: &str = include_str!("assets/agent.Dockerfile");

/// Build the default agent image (`agentry-agent:latest`) from the bundled
/// Dockerfile using the detected container engine.
pub fn image_build() -> Result<()> {
    let engine = recipe::container_engine().ok_or_else(|| {
        anyhow!(
            "no container engine found — install docker or podman, or set AGENTRY_CONTAINER_ENGINE"
        )
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
            println!(
                "   podman, or set AGENTRY_CONTAINER_ENGINE (or use runtime = \"foreground\")."
            );
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
    println!("  agentry daemon                   # start the daemon (in another terminal)");
    println!("  agentry image build              # build the agent image (once, if needed)");
    println!("  agentry start onboarding-agent   # spawn it in a container");
    println!("  agentry attach <name>            # attach and chat (name from `agentry list`)");
}
