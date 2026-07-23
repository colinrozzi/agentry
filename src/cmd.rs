//! Local (non-daemon) CLI commands: `init` and `image build`. Everything
//! stateful (start/stop/list/show/attach, recipes) goes through the daemon —
//! see `client.rs` (client side) and `daemon.rs` (server side).

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};

use crate::recipe;

/// The starter recipe seeded by `agentry init`, embedded at build time.
const ONBOARDING_RECIPE_TOML: &str = include_str!("assets/onboarding-agent.recipe.toml");
const ONBOARDING_CLAUDE_MD: &str = include_str!("assets/onboarding-agent.CLAUDE.md");
const ONBOARDING_LAUNCH_SH: &str = include_str!("assets/onboarding-agent.launch.sh");
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
    let launch_sh = dir.join("launch.sh");

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
        std::fs::write(&launch_sh, ONBOARDING_LAUNCH_SH)
            .with_context(|| format!("writing {}", launch_sh.display()))?;
        println!("seeded onboarding-agent recipe at {}", dir.display());
        println!("  recipe: {}", recipe_toml.display());
        println!("  launch: {}", launch_sh.display());
        println!("  guide:  {}", claude_md.display());
    }
    println!();
    check_container_prereqs();
    print_next_steps();
    Ok(())
}

/// The onboarding-agent runs in a container, so warn early (at `init`) if podman
/// or the agent image isn't ready.
fn check_container_prereqs() {
    let engine = match recipe::container_engine() {
        Some(e) => e,
        None => {
            println!("⚠  podman not found.");
            println!("   The onboarding-agent runs in a container — install podman (or edit");
            println!("   its launch.sh / use a `foreground` recipe to run claude directly).");
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

/// Export a recipe to a shareable bundle: a gzip'd tar of the recipe's whole
/// directory, named `<name>.recipe` by default. `agentry recipes install`
/// unpacks it on the other side.
pub fn recipes_export(name: &str, out: Option<&str>) -> Result<()> {
    let recipe = recipe::resolve(name)?;
    let dir = recipe
        .source
        .parent()
        .ok_or_else(|| anyhow!("recipe {} has no parent directory", recipe.name))?;
    let parent = dir
        .parent()
        .ok_or_else(|| anyhow!("recipe directory {} has no parent", dir.display()))?;
    let dir_name = dir
        .file_name()
        .ok_or_else(|| anyhow!("recipe directory {} has no name", dir.display()))?;
    let out = out
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(format!("{}.recipe", recipe.name)));

    let ok = Command::new("tar")
        .arg("czf")
        .arg(&out)
        .arg("-C")
        .arg(parent)
        .arg(dir_name)
        .status()
        .context("running tar")?;
    if !ok.success() {
        return Err(anyhow!("tar failed to create {}", out.display()));
    }
    println!("exported '{}' -> {}", recipe.name, out.display());
    println!(
        "share it, then on the other machine:  agentry recipes install {}",
        out.display()
    );
    Ok(())
}

/// Install a recipe bundle (from `recipes export`) into the primary recipes
/// directory, validate it, and report how to start it.
pub fn recipes_install(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("bundle not found: {}", path.display()));
    }
    let recipes_root = recipe::search_path()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("could not determine a recipes directory"))?;
    std::fs::create_dir_all(&recipes_root)
        .with_context(|| format!("creating {}", recipes_root.display()))?;

    // Peek at the bundle's top-level directory (the recipe's folder name).
    let listing = Command::new("tar")
        .arg("tzf")
        .arg(path)
        .output()
        .context("running tar tzf")?;
    if !listing.status.success() {
        return Err(anyhow!("not a readable .recipe bundle: {}", path.display()));
    }
    let top = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .next()
        .and_then(|l| l.split('/').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("bundle {} is empty", path.display()))?;

    let ok = Command::new("tar")
        .arg("xzf")
        .arg(path)
        .arg("-C")
        .arg(&recipes_root)
        .status()
        .context("running tar xzf")?;
    if !ok.success() {
        return Err(anyhow!("tar failed to extract {}", path.display()));
    }

    let installed = recipes_root.join(&top);
    let toml_path = installed.join("recipe.toml");
    if !toml_path.is_file() {
        return Err(anyhow!(
            "bundle did not contain a recipe.toml (looked in {})",
            installed.display()
        ));
    }
    let recipe = recipe::Recipe::from_path(&toml_path)
        .with_context(|| format!("installed recipe at {} does not parse", installed.display()))?;

    println!("installed '{}' at {}", recipe.name, installed.display());
    if let Some(img) = &recipe.image {
        if installed.join("Dockerfile").is_file() {
            println!("  image {img} builds automatically on first `agentry start`");
        }
    }
    println!("  start it with:  agentry start {}", recipe.name);
    Ok(())
}

/// Import a single recipe by name from a source: a local directory, a git repo,
/// or a `.recipe` bundle (local file or URL). The recipe is looked for both at
/// the source root (`<source>/<name>/`) and under a `recipes/` subdir
/// (`<source>/recipes/<name>/`), so both dedicated recipe repos and monorepos work.
pub fn recipes_import(name: &str, source: &str) -> Result<()> {
    let src_path = std::path::Path::new(source);

    // 1. A local directory that holds recipes.
    if src_path.is_dir() {
        let dir = find_recipe_dir(src_path, name)
            .ok_or_else(|| anyhow!("no recipe '{name}' under {}", src_path.display()))?;
        return bundle_and_install(&dir);
    }

    // 2. A `.recipe` bundle — a local file, or a URL to download.
    if source.ends_with(".recipe") {
        if src_path.is_file() {
            return recipes_install(src_path);
        }
        let tmp =
            std::env::temp_dir().join(format!("agentry-import-{}.recipe", std::process::id()));
        let ok = Command::new("curl")
            .args(["-fsSL", source, "-o"])
            .arg(&tmp)
            .status()
            .context("running curl")?;
        if !ok.success() {
            return Err(anyhow!("failed to download {source}"));
        }
        let r = recipes_install(&tmp);
        let _ = std::fs::remove_file(&tmp);
        return r;
    }

    // 3. Otherwise treat it as a git repo.
    import_from_git(name, source)
}

/// Look for a recipe directory named `name` at `base/<name>` or `base/recipes/<name>`.
fn find_recipe_dir(base: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    [base.join(name), base.join("recipes").join(name)]
        .into_iter()
        .find(|c| c.join("recipe.toml").is_file())
}

/// Normalize a source into a git URL. `owner/repo` and `github.com/owner/repo`
/// become `https://github.com/...`; anything with a scheme or scp-style host is
/// used as-is.
fn git_url(source: &str) -> String {
    if source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.starts_with("ssh://")
    {
        source.to_string()
    } else if let Some(rest) = source.strip_prefix("github.com/") {
        format!("https://github.com/{rest}")
    } else if source.matches('/').count() == 1 {
        format!("https://github.com/{source}")
    } else {
        source.to_string()
    }
}

fn import_from_git(name: &str, source: &str) -> Result<()> {
    let url = git_url(source);
    let tmp = std::env::temp_dir().join(format!("agentry-import-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    println!("fetching {url} …");
    let ok = Command::new("git")
        .args(["clone", "--depth", "1", &url])
        .arg(&tmp)
        .status()
        .context("running git clone (is git installed?)")?;
    if !ok.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(anyhow!("git clone failed: {url}"));
    }
    let result = match find_recipe_dir(&tmp, name) {
        Some(dir) => bundle_and_install(&dir),
        None => Err(anyhow!(
            "no recipe '{name}' in {url} (looked at the root and recipes/)"
        )),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Tar a recipe directory into a temp bundle and install it — so directory and
/// git sources reuse the same validate/report path as a `.recipe` bundle.
fn bundle_and_install(recipe_dir: &std::path::Path) -> Result<()> {
    let parent = recipe_dir
        .parent()
        .ok_or_else(|| anyhow!("recipe dir {} has no parent", recipe_dir.display()))?;
    let name = recipe_dir
        .file_name()
        .ok_or_else(|| anyhow!("recipe dir {} has no name", recipe_dir.display()))?;
    let tmp = std::env::temp_dir().join(format!("agentry-import-b-{}.recipe", std::process::id()));
    let ok = Command::new("tar")
        .arg("czf")
        .arg(&tmp)
        .arg("-C")
        .arg(parent)
        .arg(name)
        .status()
        .context("running tar")?;
    if !ok.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("tar failed to bundle {}", recipe_dir.display()));
    }
    let r = recipes_install(&tmp);
    let _ = std::fs::remove_file(&tmp);
    r
}

#[cfg(test)]
mod tests {
    use super::git_url;

    #[test]
    fn git_url_normalizes_sources() {
        assert_eq!(git_url("owner/repo"), "https://github.com/owner/repo");
        assert_eq!(
            git_url("github.com/owner/repo"),
            "https://github.com/owner/repo"
        );
        assert_eq!(
            git_url("https://github.com/owner/repo"),
            "https://github.com/owner/repo"
        );
        assert_eq!(
            git_url("git@github.com:owner/repo.git"),
            "git@github.com:owner/repo.git"
        );
        // a deeper path isn't a bare owner/repo — left as-is (used verbatim).
        assert_eq!(git_url("example.com/a/b/c"), "example.com/a/b/c");
    }
}
