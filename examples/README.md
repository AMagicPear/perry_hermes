# Perry Hermes Examples

This directory contains ready-to-use configuration templates and skill examples
to help you get started quickly.

## Directory Layout

```text
examples/
  README.md                 ← you are here
  config/
    perry_hermes.toml       ← main config (providers, agent, gateway)
  skills/
    rust-core-style/
      SKILL.md              ← sample skill with YAML frontmatter
```

## Quick Start

### 1. CLI (terminal TUI)

```bash
# Copy the config template
cp examples/config/perry_hermes.toml perry_hermes.toml

# Edit perry_hermes.toml — set your API key env var, provider, and model
# Then run:
perry-hermes
```

The CLI searches for config in this order:

1. `--config <path>` (explicit flag)
2. `~/.perry_hermes/config.toml` (user-global)
3. `./perry_hermes.toml` (project-local, recommended for beginners)

### 2. Gateway (Telegram / QQ Bot)

```bash
# Copy the config template and uncomment the [gateway.*] sections
cp examples/config/perry_hermes.toml ~/.perry_hermes/config.toml

# Set the required environment variables:
#   TELEGRAM_BOT_TOKEN     — from @BotFather
#   QQ_BOT_APP_ID          — from QQ Open Platform
#   QQ_BOT_APP_SECRET      — from QQ Open Platform
# And your LLM provider API key (e.g. OPENAI_API_KEY)

# Run the gateway
perry-hermes gateway run
```

See the `[gateway.telegram]` and `[gateway.qqbot]` sections in
[perry_hermes.toml](config/perry_hermes.toml) for the full config reference.

### 3. Skills

Skills are `SKILL.md` files that teach the agent domain-specific knowledge.
Place them under `$PERRY_HERMES_HOME/skills/`:

```text
~/.perry_hermes/skills/
  coding/
    rust-core-style/SKILL.md
  writing/
    concise-prose/SKILL.md
```

Each skill needs a YAML frontmatter header with `name` and `description`:

```yaml
---
name: my-skill
description: One-line summary shown to the agent in the system prompt.
---

# My Skill

Instructions go here in Markdown.
```

See [skills/rust-core-style/SKILL.md](skills/rust-core-style/SKILL.md) for a
complete example.

## Provider Examples

The main config file (`perry_hermes.toml`) includes commented-out blocks for
common providers. Here is a quick reference:

| Provider | `kind` | `base_url` | Notes |
|---|---|---|---|
| OpenAI | `openai` | `https://api.openai.com/v1` | Default |
| DeepSeek | `openai` | `https://api.deepseek.com/v1` | OpenAI-compatible |
| Ollama (local) | `openai` | `http://localhost:11434/v1` | No API key needed |
| Anthropic | `anthropic` | _(omit for default)_ | Claude models |
| MiniMax | `openai` or `anthropic` | `https://api.minimaxi.com/v1` | Mimo models |
| Echo (testing) | `echo` | _(none)_ | Repeats input, no API key |

## Environment Variables

API keys are read from environment variables. You can either:

- Export them in your shell (`.bashrc`, `.zshrc`, etc.)
- Use [direnv](https://direnv.net/) with an `.envrc` file (see `.envrc.example`
  at the project root)

```bash
# Minimal example for OpenAI-compatible providers
export OPENAI_API_KEY=sk-your-key-here

# Optional: override base URL and model
export OPENAI_BASE_URL=https://api.deepseek.com/v1
export OPENAI_MODEL=deepseek-chat
```

For gateway platforms:

```bash
export TELEGRAM_BOT_TOKEN=123456:ABC-DEF...
export QQ_BOT_APP_ID=123456789
export QQ_BOT_APP_SECRET=your-secret
```

## Offline / Smoke Test

No API key? Use the `echo` provider for an offline smoke test:

```toml
[[providers]]
name = "local"
kind = "echo"

[[providers.models]]
name = "echo"
context_window_size = 128_000

[agent]
default_provider = "local"
default_model = "echo"
```

The echo provider simply repeats your input — useful for verifying that the
CLI, TUI, and tool system work before connecting to a real LLM.
