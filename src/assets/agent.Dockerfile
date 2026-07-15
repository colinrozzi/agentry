# The default agentry agent image (`agentry-agent:latest`).
#
# Built by `agentry image build`. Runs the `claude` CLI inside a container; at
# spawn time agentry mounts the caller's ~/.claude (for auth) and the session's
# working directory (at /work). If the recipe sets `control_socket = true`, the
# host control socket is mounted at /run/agentry.sock and the in-image `agentry`
# binary (below) acts as a client of the host daemon. See src/recipe.rs.

# --- Build the agentry client binary from source (debian-native glibc) ---
FROM rust:slim-bookworm AS build
RUN cargo install --git https://github.com/colinrozzi/agentry --locked --root /usr/local agentry

# --- Runtime image ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        git \
        tmux \
        jq \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Native Claude Code install (no Node needed). Installs to /root/.local/bin.
RUN curl -fsSL https://claude.ai/install.sh | bash
ENV PATH="/root/.local/bin:${PATH}"
RUN claude --version

# The install writes a stub ~/.claude.json (a build-time machine/user id, but no
# `hasCompletedOnboarding`). Leaving it would force onboarding at spawn even
# though the mounted token is valid — so drop it. At spawn agentry copies the
# caller's real ~/.claude.json in (see src/recipe.rs::container_steps).
RUN rm -f /root/.claude.json

# jq program agentry runs at spawn to trust the working directory (/work), so the
# agent skips the "trust this folder?" prompt.
RUN printf '%s\n' '(.projects //= {}) | .projects["/work"].hasTrustDialogAccepted = true' \
        > /etc/agentry-trust.jq

# The agentry client, for agents that get the control socket.
COPY --from=build /usr/local/bin/agentry /usr/local/bin/agentry
RUN agentry --version

WORKDIR /work
CMD ["/bin/bash"]
