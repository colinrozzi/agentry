# agentry-agent — the default container image for agentry sessions.
#
# Ships Claude Code, jq, and the agentry client (so an agent with the control
# socket can drive the host fleet). No tmux, no entrypoint: recipe launch scripts
# run the agent as the container's PID 1 and attach with `podman attach`. Nothing
# user-specific is baked in — launch.sh mounts ~/.claude and copies ~/.claude.json
# at spawn.

# --- Build the agentry client (debian-native glibc) ---
FROM rust:slim-bookworm AS build
RUN cargo install --git https://github.com/colinrozzi/agentry --locked --root /usr/local agentry

# --- Runtime image ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        git \
        jq \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Native Claude Code install (no Node needed).
RUN curl -fsSL https://claude.ai/install.sh | bash
ENV PATH="/root/.local/bin:${PATH}"
RUN claude --version
# The installer writes a stub ~/.claude.json (no hasCompletedOnboarding); drop it
# so launch scripts copy the caller's real one in cleanly.
RUN rm -f /root/.claude.json

# Helper used by launch scripts: trust a working directory so Claude skips the
# "trust this folder?" prompt.  Usage: agentry-trust [dir]  (default /work)
RUN cat > /usr/local/bin/agentry-trust <<'EOF' && chmod +x /usr/local/bin/agentry-trust
#!/bin/sh
d="${1:-/work}"
f="$HOME/.claude.json"
[ -f "$f" ] || printf '{}' > "$f"
t="$(mktemp)"
jq --arg d "$d" '(.projects //= {}) | .projects[$d].hasTrustDialogAccepted = true' "$f" > "$t" && mv "$t" "$f"
EOF

COPY --from=build /usr/local/bin/agentry /usr/local/bin/agentry
RUN agentry --version

WORKDIR /work
CMD ["/bin/bash"]
