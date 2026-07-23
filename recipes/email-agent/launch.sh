#!/bin/sh
# launch.sh — bring up the email agent (an interactive Claude session that can
# read and send your mail via IMAP/SMTP). The agent runs as the container's
# PID 1; attach with `agentry attach`. Your mail credentials are mounted
# read-only and read inside — never in podman's env/argv.
#
# Requires ~/.config/agentry/secrets/email.env with:
#   EMAIL_USER=you@example.com
#   EMAIL_APP=<app password>          (Gmail: an "app password"; 2FA required)
# For a non-Gmail provider, also set IMAP_HOST / SMTP_HOST in that file.
set -eu

IMAGE="${AGENTRY_IMAGE:-email-agent:latest}"
EMAIL_ENV="$HOME/.config/agentry/secrets/email.env"

mkdir -p "$AGENTRY_WORKDIR"
if [ -n "${AGENTRY_CLAUDE_MD:-}" ]; then
    cp "$AGENTRY_CLAUDE_MD" "$AGENTRY_WORKDIR/CLAUDE.md"
fi

CLAUDE_JSON_MOUNT=""
if [ -f "$AGENTRY_CLAUDE_JSON" ]; then
    CLAUDE_JSON_MOUNT="-v $AGENTRY_CLAUDE_JSON:/run/host-claude.json:ro"
fi
EMAIL_MOUNT=""
if [ -f "$EMAIL_ENV" ]; then
    EMAIL_MOUNT="-v $EMAIL_ENV:/run/email.env:ro"
fi

# shellcheck disable=SC2086
podman run -dit --name "$AGENTRY_SESSION" \
    -e TERM=xterm-256color \
    -e AGENTRY_MESSAGE="${AGENTRY_MESSAGE:-}" \
    -v "$AGENTRY_CLAUDE_HOME:/root/.claude" \
    $CLAUDE_JSON_MOUNT $EMAIL_MOUNT \
    -v "$AGENTRY_WORKDIR:/work" \
    "$IMAGE" \
    sh -c '
        if [ -f /run/host-claude.json ]; then cp /run/host-claude.json "$HOME/.claude.json"; fi
        agentry-trust /work
        set -a; [ -f /run/email.env ] && . /run/email.env; set +a
        if [ -n "$AGENTRY_MESSAGE" ]; then exec claude "$AGENTRY_MESSAGE"; else exec claude; fi
    '
