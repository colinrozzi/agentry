# shellcheck shell=bash
# Source this file to get agentry-* shell functions:
#   source /home/colin/work/actors/agentry/scripts/agentry-helpers.sh
#
# Talks to a running agentry-actor (default http://127.0.0.1:8090).
# Start the actor with:
#   theater spawn /home/colin/work/actors/agentry/actor/manifest.toml

: "${AGENTRY_ACTOR_URL:=http://127.0.0.1:8090}"
: "${AGENTRY_RECIPES:=$HOME/.config/agentry/recipes}"
: "${AGENTRY_WORKSPACES:=$HOME/work/agentry-workspaces}"
: "${AGENTRY_IMAGE:=localhost/agent-poc:latest}"
: "${INBOX_TOKEN_FILE:=$HOME/.config/inbox/token}"

agentry-spawn() {
  local recipe="${1:?usage: agentry-spawn <recipe>}"
  local recipe_dir="$AGENTRY_RECIPES/$recipe"
  local claude_md="$recipe_dir/CLAUDE.md"
  if [ ! -f "$claude_md" ]; then
    echo "agentry-spawn: no CLAUDE.md at $claude_md" >&2
    return 1
  fi
  if [ ! -f "$INBOX_TOKEN_FILE" ]; then
    echo "agentry-spawn: missing inbox token at $INBOX_TOKEN_FILE" >&2
    return 1
  fi
  local inbox_token
  inbox_token=$(<"$INBOX_TOKEN_FILE")

  local workspace="$AGENTRY_WORKSPACES/$recipe"
  if [ ! -d "$workspace" ]; then
    echo "agentry-spawn: no workspace at $workspace (clone the repo there first)" >&2
    return 1
  fi

  # Hand-rolled JSON. None of these fields can contain JSON-special chars
  # in normal usage (token is hex, paths don't have quotes/backslashes).
  # If that assumption breaks, switch to jq.
  local spec
  printf -v spec '{
    "image": "%s",
    "name": "agent-%s",
    "env": [["INBOX_TOKEN","%s"],["TERM","xterm-256color"]],
    "mounts": [
      {"source":"%s/.claude.json","target":"/root/.claude.json","read_only":false},
      {"source":"%s/.claude","target":"/root/.claude","read_only":false},
      {"source":"%s","target":"/workspace","read_only":false},
      {"source":"%s","target":"/workspace/CLAUDE.md","read_only":true},
      {"source":"%s/.config/gh","target":"/root/.config/gh","read_only":false},
      {"source":"%s/.ssh","target":"/root/.ssh","read_only":true}
    ],
    "cmd": ["claude", "begin"],
    "tty": true,
    "interactive": true
  }' "$AGENTRY_IMAGE" "$recipe" "$inbox_token" "$HOME" "$HOME" "$workspace" "$claude_md" "$HOME" "$HOME"

  curl -sS -X POST -H 'Content-Type: application/json' \
    --data "$spec" \
    "$AGENTRY_ACTOR_URL/sessions"
  echo
}

agentry-list() {
  curl -sS "$AGENTRY_ACTOR_URL/sessions"
  echo
}

agentry-show() {
  local recipe="${1:?usage: agentry-show <recipe>}"
  curl -sS -w "\n[http=%{http_code}]\n" "$AGENTRY_ACTOR_URL/sessions/agent-$recipe"
}

agentry-stop() {
  local recipe="${1:?usage: agentry-stop <recipe>}"
  curl -sS -X DELETE -w "[http=%{http_code}]\n" "$AGENTRY_ACTOR_URL/sessions/agent-$recipe"
}

# Attach the current terminal to the agent's foreground process (claude).
# Detach with Ctrl-p Ctrl-q (the default podman detach sequence). Ctrl-c
# will kill claude, which kills the container.
agentry-attach() {
  local recipe="${1:?usage: agentry-attach <recipe>}"
  exec podman attach "agent-$recipe"
}

# Open a bash shell inside the agent's container (separate from the
# foreground claude). Useful for poking around without disturbing the
# session.
agentry-exec() {
  local recipe="${1:?usage: agentry-exec <recipe>}"
  exec podman exec -it "agent-$recipe" bash
}
