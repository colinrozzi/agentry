# The default agentry agent image (`agentry-agent:latest`).
#
# Built by `agentry image build`. Runs the `claude` CLI inside a container; at
# spawn time agentry mounts the caller's ~/.claude (for auth) and the session's
# working directory (at /work). See the container runtime in src/recipe.rs.

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        git \
        tmux \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Native Claude Code install (no Node needed). Installs to /root/.local/bin.
RUN curl -fsSL https://claude.ai/install.sh | bash
ENV PATH="/root/.local/bin:${PATH}"
RUN claude --version

WORKDIR /work
CMD ["/bin/bash"]
