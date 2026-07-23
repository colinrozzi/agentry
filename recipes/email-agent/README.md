# email-agent

An interactive Claude session that can **read and send your email** over
IMAP/SMTP. Attach to it and it summarizes your unread mail, drafts replies, and
sends them — always with your confirmation.

## Setup

1. **Provide credentials** — create `~/.config/agentry/secrets/email.env`:
   ```
   EMAIL_USER=you@example.com
   EMAIL_APP=xxxxxxxxxxxxxxxx
   ```
   `EMAIL_APP` is an **app password** (not your login password). For Gmail:
   enable 2-Step Verification, then create one at
   <https://myaccount.google.com/apppasswords>. For a non-Gmail provider, also add
   `IMAP_HOST=...` and `SMTP_HOST=...` to that file.

2. **Base image** — this recipe builds `FROM agentry-agent`, so build the base once:
   ```
   agentry image build
   ```

## Run

```sh
agentry start email-agent     # builds the recipe image on first start
agentry attach <name>         # from `agentry list`
```

It boots, reads your unread, and summarizes it. Sends are gated behind your
explicit "yes."

## What's in here

- `recipe.toml` — metadata (declares `image = "email-agent:latest"`)
- `launch.sh` — mounts your `~/.claude` + `email.env`, runs the agent as PID 1
- `Dockerfile` — `agentry-agent` + python + the `email` CLI
- `email` — a ~200-line IMAP/SMTP CLI (`unread`/`recent`/`show`/`search`/`send`)
- `CLAUDE.md` — the agent's brief
