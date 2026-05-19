# agentry

A small CLI tool for managing local AI agent sessions: recipes, jj workspaces, and tmux-backed lifecycle.

Designed for the one-human, one-machine case — you sit at your laptop, want to spin up agents on demand for specific tasks or as long-lived specialists, see what's running, and tear them down cleanly. No service, no central coordinator; just a tool.

## Commands

```sh
agentry recipes list                 # enumerate recipes in the search path
agentry recipes show <name|path>     # show one recipe's metadata + paths

agentry start <recipe> [--repo <p>] [--for <ticket>]
agentry list                         # what's running (queries tmux for liveness)
agentry show <name>                  # full state for one session
agentry attach <name>                # tmux attach -t agent-<name>
agentry stop <name>                  # kill tmux, forget workspace, delete state
```

## The model

### Recipe

A recipe is the identity template for an agent. It's a `recipe.toml` file that can live anywhere on disk:

```toml
name = "inbox-dev"
description = "Mail server specialist"
repository = "/home/colin/work/actors/inbox"
claude_md_path = "./CLAUDE.md"
```

Two shapes naturally emerge:
- **Bound recipes** (`inbox-dev`, `theater-dev`, etc): repository fixed in the recipe. Long-lived specialists.
- **Generic recipes** (`coding`, `review`, `investigator`): no fixed repository; specify at spawn time with `--repo`. Short-lived task workers.

The directory containing `recipe.toml` is purely organizational; the tool only cares about the file and the paths it references.

### Search path

`agentry start <name>` and `agentry recipes list` look in (in order):
1. The `AGENTRY_RECIPES` env var (colon-separated, like `$PATH`), if set
2. `$XDG_CONFIG_HOME/agentry/recipes/` (typically `~/.config/agentry/recipes/`)

You can also bypass the search path: `agentry start /tmp/my-recipe.toml`.

### Session lifecycle

When you `agentry start <recipe>`:
1. Create a jj workspace at `~/work/agentry-sessions/<uuid>/` based on `main` of the recipe's repository (named `agent-<short>` in `jj workspace list`)
2. Copy the recipe's `CLAUDE.md` into the workspace root
3. Start a detached tmux session named `agent-<short>` running `claude` in the workspace
4. Write a state file at `~/.local/state/agentry/<short>.json` with the session's metadata

`agentry list` reads state files + queries tmux to show what's running. `agentry attach <name>` is a shortcut for `tmux attach -t agent-<name>`. `agentry stop <name>` kills the tmux session, forgets the jj workspace, deletes the worktree directory, and removes the state file.

The repo must be jj-colocated. We use `jj workspace add` rather than `git worktree add` so that multiple sessions can coexist on top of `main` without git's "one worktree per branch" restriction.

## Build & install

```sh
nix develop --command cargo build       # debug build at ./target/debug/agentry
nix build                               # release build via flake

nix profile install /home/colin/work/agentry   # install into user profile
nix profile upgrade agentry                    # pick up local changes
```

## Why not a service?

For the use case (one human, one machine, local-only), a CLI tool fits the shape better than a service: nothing to keep running, no API to maintain, no auth surface. If we later want remote management or multi-machine fleet views, we can upgrade. Today's reality is simpler.
