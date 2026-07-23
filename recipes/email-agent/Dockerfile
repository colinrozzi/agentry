# email-agent image: agentry-agent (claude) + python + a tiny `email` CLI.
# Built automatically on first `agentry start email-agent` (recipe sets `image`).
FROM agentry-agent:latest
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY email /usr/local/bin/email
RUN chmod +x /usr/local/bin/email && python3 /usr/local/bin/email --help >/dev/null
