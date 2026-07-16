#!/bin/sh
# launch.sh — bring up the onboarding agent.
#
# agentry runs this on the host when you `agentry start onboarding-agent`, with
# context in AGENTRY_* env vars (real shell — no template quoting to fight). It
# starts one container whose PID 1 provisions Claude and then *becomes* the agent
# (via `exec`), so `agentry attach` == `podman attach` connects straight to it.
# To run a different image, mount other things, or use a different harness, edit
# this file — nothing here is baked into agentry.
set -eu

IMAGE="agentry-agent:latest"

# Host working directory (bind-mounted at /work), seeded with the guide.
mkdir -p "$AGENTRY_WORKDIR"
if [ -n "${AGENTRY_CLAUDE_MD:-}" ]; then
    cp "$AGENTRY_CLAUDE_MD" "$AGENTRY_WORKDIR/CLAUDE.md"
fi

# Mount your ~/.claude.json read-only (if present) so the container can copy in
# your onboarding state without writing back to the real file.
CLAUDE_JSON_MOUNT=""
if [ -f "$AGENTRY_CLAUDE_JSON" ]; then
    CLAUDE_JSON_MOUNT="-v $AGENTRY_CLAUDE_JSON:/run/host-claude.json:ro"
fi

# Start the container. PID 1 is a shell that provisions credentials + trust, then
# `exec claude` — so the agent is PID 1 and `podman attach` reaches it directly.
# -dit: detached, but with a tty + stdin so attach works. Named $AGENTRY_SESSION
# so agentry's attach/status/stop find it. Mounts: ~/.claude (auth), the workdir,
# and the control socket so the guide can drive the host fleet.
# shellcheck disable=SC2086
podman run -dit --name "$AGENTRY_SESSION" \
    -e TERM=xterm-256color \
    -e AGENTRY_MESSAGE="${AGENTRY_MESSAGE:-}" \
    -v "$AGENTRY_CLAUDE_HOME:/root/.claude" \
    $CLAUDE_JSON_MOUNT \
    -v "$AGENTRY_WORKDIR:/work" \
    -v "$AGENTRY_CONTROL_SOCKET:/run/agentry.sock" -e AGENTRY_SOCKET=/run/agentry.sock \
    -w /work "$IMAGE" \
    sh -c '
        if [ -f /run/host-claude.json ]; then cp /run/host-claude.json "$HOME/.claude.json"; fi
        agentry-trust /work
        if [ -n "$AGENTRY_MESSAGE" ]; then exec claude "$AGENTRY_MESSAGE"; else exec claude; fi
    '
