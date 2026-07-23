# recipes

A small, curated collection of ready-to-use agentry recipes. Each is a
**self-contained directory** — `recipe.toml` + `launch.sh` + `CLAUDE.md` + its
`Dockerfile`/assets — so it carries everything it needs and its image builds on
first `agentry start`.

## Using them

Point agentry at this directory (all of them at once):

```sh
git clone https://github.com/colinrozzi/agentry
export AGENTRY_RECIPES="$PWD/agentry/recipes"
agentry recipes list
agentry start email-agent
```

…or grab a single one (once it's a proper file, `agentry recipes install`; a
future `agentry recipes import <name> <source>` will pull one straight from a
repo).

## Recipes

| recipe | what it is | needs |
|---|---|---|
| [`email-agent`](email-agent/) | chat with an assistant that reads and sends your email | `~/.config/agentry/secrets/email.env`, the `agentry-agent` base image |

Most recipes here build `FROM agentry-agent` (Claude Code + the agentry client),
so build that base once with `agentry image build`. Recipes reference
credentials by **path** (e.g. `~/.config/agentry/secrets/…`) and never contain
secrets — you supply your own.
