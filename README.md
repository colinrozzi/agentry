# agentry

A small CLI tool for managing local AI agent sessions: recipes, jj workspaces, and tmux-backed lifecycle.

Designed for the one-human, one-machine case — you sit at your laptop, want to spin up agents on demand for specific tasks or as long-lived specialists, see what's running, and tear them down cleanly. No service, no central coordinator; just a tool.

## Getting started

```sh
agentry init                         # seed your first recipe (onboarding-agent)
agentry start onboarding-agent       # spawn it — a plain-dir agent, no jj needed
agentry list                         # find its name
agentry attach <name>                # attach and ask it about agentry
```

`agentry init` writes a starter `onboarding-agent` recipe into your recipes
directory. It's an interactive guide to the tool *and* a worked example of the
recipe format — spawn it, attach, and it'll walk you through the rest.

## Commands

```sh
agentry init [--force]               # seed the onboarding-agent recipe

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

A recipe is the instantiation document for an agent — think of it as the agent's
Dockerfile. It's a `recipe.toml` file that can live anywhere on disk:

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

### The lifecycle engine

agentry itself knows nothing about jj or tmux. A session's whole lifecycle is a
set of **declared shell steps** the recipe can override:

| field | when it runs | default (if unset) |
|---|---|---|
| `setup` | at `start`, to provision the workspace | `jj workspace add -r main` + copy `CLAUDE.md` |
| `launch` | at `start`, to start the (detached) runtime | `tmux new-session -d … {command}` |
| `status` | at `list`/`show`, for liveness (exit 0 = alive) | `tmux has-session` |
| `attach` | at `attach`, to connect interactively | `tmux attach` |
| `teardown` | at `stop` (best-effort), to reverse setup | kill tmux, `jj workspace forget`, `rm` |
| `command` | the process run inside the session | `claude` |
| `workdir` | the session's working directory | `{sessions_root}/{uuid}` |

A recipe that declares **none** of these inherits the full jj + tmux default, so
existing recipes work untouched. Override just one verb (or all of them) to get a
different shape — a plain-directory agent, a cloud runtime, whatever. Steps are
run through `sh -c` with `{var}` placeholders substituted:

```toml
# a plain-directory agent — no jj, no repo required
name = "onboarding-agent"
description = "Meet agentry"
claude_md_path = "./CLAUDE.md"
setup    = ["mkdir -p {workdir}", "cp {claude_md} {workdir}/CLAUDE.md"]
teardown = ["tmux kill-session -t {session}", "rm -rf {workdir}"]
# launch/status/attach unset ⇒ tmux runtime by default
```

**Template variables:** `{uuid}` `{short}` `{session}` (=`agent-{short}`)
`{workdir}` `{sessions_root}` `{recipe_dir}` `{claude_md}` `{repo}` `{command}`.
A `{name}` outside this set (e.g. shell `${HOME}`) is left untouched; a *known*
variable that's referenced but unset (e.g. `{repo}` with no `repository`) is an
error at spawn time.

Steps run as shell on your machine — recipes are trusted local files you author,
same as the `claude` process they launch.

### Search path

`agentry start <name>` and `agentry recipes list` look in (in order):
1. The `AGENTRY_RECIPES` env var (colon-separated, like `$PATH`), if set
2. `$XDG_CONFIG_HOME/agentry/recipes/` (typically `~/.config/agentry/recipes/`)

You can also bypass the search path: `agentry start /tmp/my-recipe.toml`.

### Session lifecycle

When you `agentry start <recipe>`, agentry resolves the recipe's plan
(substituting `{var}` placeholders) and then:
1. Runs each `setup` step in order (default: create a jj workspace at `~/work/agentry-sessions/<uuid>/` on `main`, copy `CLAUDE.md` in). If any step fails, it runs `teardown` to roll back.
2. Runs `launch` (default: a detached tmux session `agent-<short>` running `claude` in the workspace).
3. Writes a state file at `~/.local/state/agentry/<short>.json` — including the *resolved* `status`/`attach`/`teardown` commands, so the lifecycle commands never need to re-read the recipe.

`agentry list`/`show` run each session's `status` command for liveness.
`agentry attach <name>` runs its `attach` command. `agentry stop <name>` runs the
stored `teardown` steps (best-effort) and removes the state file.

For the default (jj) workspace strategy, the repo must be jj-colocated. We use
`jj workspace add` rather than `git worktree add` so that multiple sessions can
coexist on top of `main` without git's "one worktree per branch" restriction. A
recipe that overrides `setup`/`teardown` (e.g. a plain-directory or cloud agent)
has no such requirement.

## Build & install

```sh
nix develop --command cargo build       # debug build at ./target/debug/agentry
nix build                               # release build via flake

nix profile install /home/colin/work/agentry   # install into user profile
nix profile upgrade agentry                    # pick up local changes
```

## Why not a service?

For the use case (one human, one machine, local-only), a CLI tool fits the shape better than a service: nothing to keep running, no API to maintain, no auth surface. If we later want remote management or multi-machine fleet views, we can upgrade. Today's reality is simpler.
