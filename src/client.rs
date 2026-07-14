//! CLI client: turns `agentry <verb>` into a request to the daemon, then
//! renders the response. Requires a running daemon (`agentry daemon`).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;

use crate::protocol::{ForegroundPlan, Request, Response, SessionView, PROTOCOL_VERSION};

/// Send one request to the daemon and return its `data` (or the error).
fn request(op: &str, args: Value) -> Result<Value> {
    let path = crate::protocol::socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|_| {
        anyhow!(
            "no agentry daemon running at {} — start one with `agentry daemon`",
            path.display()
        )
    })?;

    let req = Request {
        v: PROTOCOL_VERSION,
        op: op.to_string(),
        args,
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let _ = stream.shutdown(std::net::Shutdown::Write);

    let mut reader = BufReader::new(stream);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line)?;
    let resp: Response = serde_json::from_str(&resp_line)?;
    if resp.ok {
        Ok(resp.data.unwrap_or(Value::Null))
    } else {
        bail!("{}", resp.error.unwrap_or_else(|| "unknown error".into()))
    }
}

pub fn recipes_list() -> Result<()> {
    let data = request("recipes.list", json!({}))?;
    let recipes = data["recipes"].as_array().cloned().unwrap_or_default();
    if recipes.is_empty() {
        println!("(no recipes found in search path)");
        return Ok(());
    }
    let name_w = recipes
        .iter()
        .map(|r| r["name"].as_str().unwrap_or("").len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{:<width$}  description", "name", width = name_w);
    println!("{:<width$}  -----------", "----", width = name_w);
    for r in &recipes {
        println!(
            "{:<width$}  {}",
            r["name"].as_str().unwrap_or(""),
            r["description"].as_str().unwrap_or(""),
            width = name_w
        );
    }
    Ok(())
}

pub fn recipes_show(reference: &str, raw: bool) -> Result<()> {
    let d = request("recipes.show", json!({ "recipe": reference }))?;
    if raw {
        // Print the raw recipe.toml so a client can edit and re-`write` it.
        print!("{}", d["recipe_toml"].as_str().unwrap_or(""));
        return Ok(());
    }
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    println!("name:        {}", s(&d["name"]));
    println!("description: {}", s(&d["description"]));
    println!(
        "repository:  {}",
        d["repository"]
            .as_str()
            .unwrap_or("(none — must be supplied at spawn)")
    );
    println!("source:      {}", s(&d["source"]));
    println!(
        "claude.md:   {}",
        d["claude_md"].as_str().unwrap_or("(none)")
    );
    println!("runtime:     {}", s(&d["runtime"]));
    if d["runtime"] == "container" {
        println!(
            "image:       {}",
            d["image"]
                .as_str()
                .unwrap_or("agentry-agent:latest (default)")
        );
    } else if let Some(ov) = d["overrides"].as_array() {
        let names: Vec<&str> = ov.iter().filter_map(|v| v.as_str()).collect();
        println!(
            "lifecycle:   {}",
            if names.is_empty() {
                "default".to_string()
            } else {
                format!("custom: {}", names.join(", "))
            }
        );
    }
    Ok(())
}

/// Create or update a recipe on the host from a directory containing
/// `recipe.toml` (and optionally `CLAUDE.md`).
pub fn recipes_write(name: &str, from: &Path) -> Result<()> {
    let recipe_toml = std::fs::read_to_string(from.join("recipe.toml"))
        .with_context(|| format!("reading {}/recipe.toml", from.display()))?;
    let mut args = json!({ "name": name, "recipe_toml": recipe_toml });
    let cm = from.join("CLAUDE.md");
    if cm.exists() {
        args["claude_md"] =
            json!(std::fs::read_to_string(&cm)
                .with_context(|| format!("reading {}", cm.display()))?);
    }
    let d = request("recipes.write", args)?;
    println!(
        "wrote recipe '{}' to {}",
        name,
        d["path"].as_str().unwrap_or("?")
    );
    Ok(())
}

/// Delete a recipe from the host by name.
pub fn recipes_delete(name: &str) -> Result<()> {
    request("recipes.delete", json!({ "name": name }))?;
    println!("removed recipe '{}'", name);
    Ok(())
}

pub fn list() -> Result<()> {
    let data = request("session.list", json!({}))?;
    let sessions: Vec<SessionView> = serde_json::from_value(data["sessions"].clone())?;
    if sessions.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }
    let name_w = sessions
        .iter()
        .map(|s| s.session.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let recipe_w = sessions
        .iter()
        .map(|s| s.session.recipe_name.len())
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
    for s in &sessions {
        println!(
            "{:<nw$}  {:<rw$}  {:<8}  {:<6}  {}",
            s.session.name,
            s.session.recipe_name,
            s.status,
            s.session.linked_ticket.as_deref().unwrap_or("-"),
            s.session.started_at,
            nw = name_w,
            rw = recipe_w
        );
    }
    Ok(())
}

pub fn show(name: &str) -> Result<()> {
    let d = request("session.show", json!({ "name": name }))?;
    let v: SessionView = serde_json::from_value(d)?;
    let s = v.session;
    println!("name:          {}", s.name);
    println!("uuid:          {}", s.uuid);
    println!("status:        {}", v.status);
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

pub fn stop(name: &str) -> Result<()> {
    let d = request("session.stop", json!({ "name": name }))?;
    println!(
        "stopped {} (teardown complete, state removed)",
        d["name"].as_str().unwrap_or(name)
    );
    Ok(())
}

pub fn attach(name: &str) -> Result<()> {
    let d = request("session.attach", json!({ "name": name }))?;
    let cmd = d["attach_cmd"]
        .as_str()
        .ok_or_else(|| anyhow!("daemon returned no attach command"))?;
    // Run it locally so it inherits this terminal's tty.
    let status = Command::new("sh").arg("-c").arg(cmd).status()?;
    if !status.success() {
        bail!("attach failed (exit {:?})", status.code());
    }
    Ok(())
}

pub fn start(recipe: &str, repo: Option<&str>, ticket: Option<&str>) -> Result<()> {
    let mut args = json!({ "recipe": recipe });
    if let Some(r) = repo {
        args["repo"] = json!(r);
    }
    if let Some(t) = ticket {
        args["for"] = json!(t);
    }
    let d = request("session.start", args)?;

    // Foreground: the daemon ran setup and handed us the plan to run locally.
    if let Some(fg) = d.get("foreground") {
        let fp: ForegroundPlan = serde_json::from_value(fg.clone())?;
        println!(
            "starting {} (recipe={}) in {}",
            fp.name,
            fp.recipe_name,
            fp.workdir.display()
        );
        println!("(foreground session — it ends when you exit the agent)\n");
        let status = Command::new("sh")
            .arg("-c")
            .arg(&fp.command)
            .current_dir(&fp.workdir)
            .status();
        crate::daemon::run_steps_best_effort(&fp.teardown);
        let status = status?;
        if !status.success() {
            bail!("agent exited with status {:?}", status.code());
        }
        return Ok(());
    }

    // Container / shell: the daemon spawned it.
    let name = d["name"].as_str().unwrap_or("?");
    println!(
        "spawned {} (recipe={}, {})",
        name,
        recipe,
        d["runtime"].as_str().unwrap_or("")
    );
    if let Some(repo) = d["repo"].as_str() {
        println!("  repo:     {}", repo);
    }
    if let Some(wd) = d["workdir"].as_str() {
        println!("  workdir:  {}", wd);
    }
    if let Some(sess) = d["session"].as_str() {
        println!("  session:  {}", sess);
    }
    println!("\nattach with:  agentry attach {}", name);
    Ok(())
}
