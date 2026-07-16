# agentry

A small CLI tool for running local AI agent sessions: recipes plus a fully
declarative session lifecycle.

Designed for the one-human, one-machine case — you sit at your laptop, want to spin up agents on demand for specific tasks or as long-lived specialists, see what's running, and tear them down cleanly. No service, no central coordinator; just a tool.

## Requirements

The zero-config default (`runtime = "foreground"`) just runs `claude` in your
terminal — all you need is `claude` on your PATH. To run agents in containers
(what the `onboarding-agent` does), you also need:

- **podman**.
- The **agent image**, built once with `agentry image build`. It installs the
  `claude` CLI and the `agentry` client inside; a recipe's `launch.sh` mounts your
  `~/.claude` (for auth) and a working directory in at spawn.

`agentry init` checks both and tells you what's missing.

## Getting started

```sh
agentry init                         # seed your first recipe + preflight checks
agentry image build                  # build the agent image (once; installs claude)
agentry daemon &                     # start the daemon (owns state, serves the socket)
agentry start onboarding-agent       # spawn it in a container
agentry attach <name>                # attach and chat (name from `agentry list`)
```

The `onboarding-agent` is an interactive guide to the tool *and* a worked example
of the recipe format — spawn it, attach, and it'll walk you through the rest.

## The daemon

agentry runs as a small **daemon** plus a thin **CLI client**. The daemon owns
session state and serves a Unix **control socket**; every stateful command
(`start`/`stop`/`list`/`show`/`attach`, `recipes …`) is a request to it. Start it
with `agentry daemon` (foreground; background it with `&` or a service). Without
it, those commands error with `no agentry daemon running — start one with
agentry daemon`. Only `agentry daemon`, `agentry init`, and `agentry image build`
run without the daemon.

The socket is `$AGENTRY_SOCKET` (default `$XDG_RUNTIME_DIR/agentry/agentry.sock`,
perms `0600`). That permission is the trust boundary — anyone who can write the
socket can spawn/stop agents. See [docs/daemon.md](docs/daemon.md) for the design.

### Giving an agent control of the fleet

A container recipe's `launch.sh` can mount the host control socket into the
container (`-v "$AGENTRY_CONTROL_SOCKET:/run/agentry.sock" -e AGENTRY_SOCKET=…`).
The agent image ships the `agentry` binary, so the agent can then run
`agentry list` / `agentry start` / `agentry recipes list` against the *host* fleet
— from inside its sandbox, with no host filesystem or podman-socket access. The
seeded `onboarding-agent` does this, so the guide can show you your real setup.

This is a **trust grant**: an agent with the socket can spawn and stop any of
your agents (which run arbitrary shell) and create/delete recipes. Only mount it
for agents you trust. A socket agent can `agentry recipes write` to author a new
recipe on the host — so the onboarding guide can help you *build* a recipe, then
`agentry start` it, all from inside its container.

## Commands

```sh
agentry daemon                       # run the daemon (owns state + control socket)
agentry init [--force]               # seed the onboarding-agent recipe + preflight
agentry image build                  # build the default agent image

agentry recipes list                 # enumerate recipes in the search path
agentry recipes show <name|path> [--raw]   # metadata (or the raw recipe.toml)
agentry recipes write <name> --from <dir>  # create/update a recipe (recipe.toml [+ CLAUDE.md])
agentry recipes rm <name>            # delete a recipe

agentry start <recipe> [--repo <p>] [--for <ticket>]
agentry list                         # tracked sessions + their liveness
agentry show <name>                  # full state for one session
agentry attach <name>                # connect to a session
agentry stop <name>                  # run its teardown, delete state
```

`list`/`show`/`attach`/`stop` operate on tracked (container/shell) sessions. A
`foreground` session lives only in the terminal you started it in, so it isn't
tracked.

## The model

### Recipe

A recipe is the instantiation document for an agent — think of it as the agent's
Dockerfile. It's a `recipe.toml` file that can live anywhere on disk:

```toml
name = "onboarding-agent"
description = "Meet agentry"
claude_md_path = "./CLAUDE.md"
runtime = "container"          # foreground is the default; see Runtimes below
```

Two shapes naturally emerge:
- **Bound recipes** (`inbox-dev`, `theater-dev`, etc): a `repository` fixed in the recipe. Long-lived specialists.
- **Generic recipes** (`coding`, `review`, `investigator`): no fixed repository; specify at spawn time with `--repo`. Short-lived task workers.

The directory containing `recipe.toml` is purely organizational; the tool only cares about the file and the paths it references.

### Runtimes

A recipe's `runtime` decides how the agent runs:

- **`foreground`** (default) — runs `command` (default `claude`) in your terminal,
  in a fresh working directory, tearing down on exit. Zero dependencies, zero
  config; not tracked.
- **`container`** — agentry runs the recipe's **`launch.sh`** on `start` (which
  does the `podman run`, mounts, credentials, and starts the agent as PID 1), then
  owns the generic verbs: `attach` is `podman attach`, `list` checks the container
  is running, `stop` is `podman rm -f` — all keyed on the container name, which
  `launch.sh` sets to `$AGENTRY_SESSION`. Tracked by `list`/`attach`/`stop`.
- **`shell`** — you declare the whole lifecycle as shell steps (below). The escape
  hatch for tmux, jj workspaces, cloud runners — anything.

A `container` recipe is metadata plus a script — everything container-specific
lives in `launch.sh`, so there's nothing to configure here and nothing baked into
agentry:

```toml
# container recipe: recipe.toml (this) + launch.sh (next to it) + CLAUDE.md
name = "coding"
claude_md_path = "./CLAUDE.md"
runtime = "container"
# launch defaults to `sh {recipe_dir}/launch.sh`; override any verb if you need to
```

`launch.sh` gets its context as `AGENTRY_*` environment variables — e.g.
`AGENTRY_SESSION` (the container name to use), `AGENTRY_WORKDIR`,
`AGENTRY_CLAUDE_HOME`, `AGENTRY_CLAUDE_JSON`, `AGENTRY_CONTROL_SOCKET`,
`AGENTRY_MESSAGE`. It does the `podman run` however it likes; to use a different
image, mount other things, or run a different harness, you edit that one file.
See the seeded `onboarding-agent/launch.sh` for a worked example.

### The `shell` runtime — declared steps

`runtime = "shell"` gives you the full declarative lifecycle. Each field is a
shell step (or list of steps) run through `sh -c`, with `{var}` placeholders
substituted:

| field | when it runs | default (if unset) |
|---|---|---|
| `command` | the process run in the session | `claude` |
| `workdir` | the session's working directory | `{sessions_root}/{uuid}` |
| `setup` | at `start`, to provision the workdir | `mkdir -p {workdir}` |
| `launch` | at `start`, to background the runtime | — (required) |
| `status` | at `list`/`show`, for liveness (exit 0 = alive) | — (⇒ unknown) |
| `attach` | at `attach`, to connect interactively | — (⇒ no attach) |
| `teardown` | at `stop` (best-effort), to reverse setup | `rm -rf {workdir}` |

A jj-workspace + tmux specialist, for example:

```toml
name = "inbox-dev"
repository = "/path/to/actors/inbox"
claude_md_path = "./CLAUDE.md"
runtime  = "shell"
setup    = ["jj -R {repo} workspace add -r main --name {session} {workdir}",
            "cp {claude_md} {workdir}/CLAUDE.md"]
launch   = "tmux new-session -d -s {session} -c {workdir} {command}"
status   = "tmux has-session -t {session}"
attach   = "tmux attach -t {session}"
teardown = ["tmux kill-session -t {session}",
            "jj -R {repo} workspace forget {session}", "rm -rf {workdir}"]
```

**Template variables:** `{uuid}` `{short}` `{session}` (=`agent-{short}`)
`{workdir}` `{sessions_root}` `{recipe_dir}` `{claude_md}` `{claude_home}`
`{claude_json}` `{control_socket}` `{repo}` `{command}`. (Each is also exported to
`setup`/`launch` as `AGENTRY_<UPPERCASE>`, for launch scripts.)
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
(substituting `{var}` placeholders) and runs `setup`; if any step fails, it runs
`teardown` to roll back. Then, by runtime:

- **container / shell**: it runs `launch` (which backgrounds the runtime and
  returns — for `container`, that's your `launch.sh` doing the `podman run`), then
  writes a state file at `~/.local/state/agentry/<short>.json` including the
  *resolved* `status`/`attach`/`teardown` commands — so `list`/`attach`/`stop`
  never need to re-read the recipe. `list`/`show` run `status` for liveness;
  `attach` runs `attach`; `stop` runs the stored `teardown` (best-effort) and
  removes the state. For `container` those are generic `podman` commands keyed on
  the container name; for `shell` they're the recipe's own steps.
- **foreground**: it runs `command` attached to your terminal and runs `teardown`
  when the process exits. No state file — the session lives only as long as the
  process.

Per-session working directories are created under the **sessions root**:
`AGENTRY_SESSIONS` if set, else the XDG data dir (`~/.local/share/agentry/sessions`).
A `container` recipe's `launch.sh` bind-mounts that workdir wherever it likes
(the seeded one uses `/work`).

## Build & install

```sh
nix develop --command cargo build       # debug build at ./target/debug/agentry
nix build                               # release build via flake

nix profile install /path/to/agentry    # install into user profile
nix profile upgrade agentry              # pick up local changes
```

## Why not a service?

For the use case (one human, one machine, local-only), a CLI tool fits the shape better than a service: nothing to keep running, no API to maintain, no auth surface. If we later want remote management or multi-machine fleet views, we can upgrade. Today's reality is simpler.
