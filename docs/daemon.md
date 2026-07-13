# agentry daemon + control socket — design

Status: **proposed** (design only; no implementation yet)

## Summary

agentry gains a long-running **daemon** that owns all session and recipe state
and exposes a **Unix-socket control API**. The CLI becomes a thin client. A
containerized agent can be handed *just the socket* to manage the fleet and
author recipes — with no host-filesystem access and no docker socket.

Decisions locked in the design discussion:
- **Daemon owns state** (not a coordinator over files).
- **Started manually** (`agentry daemon`), not auto-spawned or a system service.
- **v1 scope = control ops + recipe CRUD** (attach stays a local exec).

## Consequence, up front

Because the daemon *owns* state and starts *manually*, **the daemon is required**
for stateful operations. Every `agentry list/show/start/stop` and `recipes …`
talks to the daemon; with none running they error clearly:

```
$ agentry list
error: no agentry daemon running — start one with `agentry daemon`
```

Getting-started gains a step:

```sh
agentry daemon            # start it (foreground; background with & or a unit)
agentry recipes list      # works, via the socket
agentry start coding --repo ~/x
```

Only these need no daemon: `agentry daemon` (starts it) and `agentry image build`
(local). This is a deliberate move away from "just a stateless CLI" toward a
client/daemon tool — the cost of a real control plane.

## Architecture

One binary, two roles:

- **`agentry daemon`** — the server. Owns an in-memory registry of sessions,
  persists it to a daemon-private store, reads/writes recipe files in the search
  path, and executes lifecycle plans (`setup`/`launch`/`status`/`teardown`) using
  the exact same runtime machinery as today (container/foreground/shell).
- **`agentry <verb>`** — the client. Connects to `$AGENTRY_SOCKET`, sends one
  request, renders the response.

The runtime engine (recipes, `Plan`, the container/shell/foreground execution)
is unchanged — it just moves *behind* the daemon.

## The socket

- Default `$XDG_RUNTIME_DIR/agentry/agentry.sock`; override with `AGENTRY_SOCKET`;
  permissions `0600`.
- **Security model = the socket permission.** Whoever can write the socket can
  spawn/stop agents and write recipes — and recipes run arbitrary shell, so it is
  effectively "run anything as you." It's a shell-level trust grant. Mounting it
  into a container trusts that container with your whole fleet and your recipe
  files. Fine for a trusted onboarding/manager agent; **never** hand it to an
  untrusted worker. (Future: capability scoping — read-only sockets, per-op
  allowlists.)

## Protocol

Minimal, no framework: Unix stream socket, newline-delimited JSON, one request →
one response, with a version tag for handshake/skew detection.

```
→ {"v":1,"op":"session.start","args":{"recipe":"coding","repo":"/x"}}
← {"ok":true,"data":{"name":"a1b2c3d4","session":"agent-a1b2c3d4"}}

→ {"v":1,"op":"session.list"}
← {"ok":true,"data":{"sessions":[ ... ]}}
```

v1 ops:

| op | args | notes |
|---|---|---|
| `recipes.list` | — | |
| `recipes.show` | `{name}` | resolved recipe + runtime |
| `recipes.write` | `{name, recipe_toml, claude_md?}` | daemon validates it resolves, then writes to the search path |
| `recipes.delete` | `{name}` | |
| `session.list` | — | |
| `session.show` | `{name}` | |
| `session.start` | `{recipe, repo?, for?}` | daemon runs setup+launch |
| `session.stop` | `{name}` | daemon runs stored teardown |
| `session.attach` | `{name}` | returns the resolved attach command; the **client execs it locally** (attach needs the caller's tty) |

## State ownership

- The daemon is the **sole owner and writer** of session state. Clients never
  touch state files. On startup it loads its persisted registry and reconciles
  liveness (runs each session's `status`). Single writer ⇒ no file races,
  accurate liveness, room for future concurrency limits.
- **Recipes stay as files** in the search path (hand-editable, VCS-friendly). The
  daemon *mediates* read/write via `recipes.*`, so socket clients — including
  containers with no filesystem mount — can create and edit them.

## Container integration

- A recipe opt-in — `control_socket = true` — makes the container runtime
  bind-mount the host socket into the container and set `AGENTRY_SOCKET`.
- The **agent image ships the `agentry` binary**, so the in-container agent is a
  client of the host daemon. A protocol version handshake guards image-vs-daemon
  skew.
- So a containerized onboarding/fleet-manager agent can run
  `agentry recipes list/write` and `agentry list/start/stop` against the *host*
  fleet — from inside its sandbox, no host filesystem, no docker socket.

## What this does / doesn't solve for onboarding

- **Solves:** a containerized guide can now inspect the fleet *and create/edit
  recipes* (`recipes.write`) — the configuration gap that made a containerized
  onboarding agent weak.
- **Doesn't solve:** inspecting the arbitrary host environment (what's installed,
  files outside agentry). The socket is scoped to agentry operations. For a guide
  that needs broad host insight, `runtime = "foreground"` is still the better
  fit. The daemon and foreground-onboarding are **complementary**, not either/or.

## Phasing

1. **Daemon + control ops.** `agentry daemon`, the socket, `session.*` +
   `recipes.list/show`. Refactor the CLI verbs into socket clients; clear
   "no daemon" error. Daemon owns/persists the registry.
2. **Recipe CRUD + containers.** `recipes.write/delete`; `control_socket` mount;
   ship `agentry` in the agent image; version handshake.
3. **Later.** Capability scoping; a systemd/launchd user unit; attach/log
   streaming over the socket; the socket → TCP+TLS step for multi-machine.

## Open questions

- `agentry daemon` foreground only, or a `--detach` convenience? (Lean:
  foreground, document `&`/unit.)
- Persisted registry: one file vs per-session files (daemon-owned)?
- `recipes.write` validation: reject recipes that don't resolve (unknown runtime,
  unresolved `{var}`)?
- Recipe field name: `control_socket` vs `socket` vs `daemon`.
- Do we keep *any* no-daemon direct fallback, or fully require the daemon? (The
  locked decision implies fully require it.)
