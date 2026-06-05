# Phase 9 — Config File and Skills Design

**Date:** 2026-06-05
**Status:** Config file partially implemented; Skills runtime loading deferred.

## 1. Goal

Add a TOML config file so provider and agent settings no longer need to be spread across many environment variables and CLI flags. Reserve a small `[skills]` config surface for the next step, but do not load Markdown skill files in this implementation slice.

## 2. Config Shape

```toml
[provider]
kind = "anthropic" # openai | anthropic | echo
api_key_env = "ANTHROPIC_API_KEY"
model = "mimo-v2.5"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[provider.thinking]
mode = "off" # off | manual | adaptive
budget_tokens = 8000
display = "summarized"
effort = "medium"

[agent]
max_iterations = 10
disabled_toolsets = []
system_prompt = "optional full replacement prompt"

[skills]
enabled = ["rust"]
paths = ["./skills"]
```

## 3. Behavior

- `hermes --config hermes.toml` loads `HermesConfig` from TOML.
- CLI flags `--model`, `--base-url`, `--max-iterations`, `--disabled-toolsets`, and `--cwd` override overlapping config/default values.
- If `--config` is omitted, existing env-driven behavior remains unchanged.
- Anthropic thinking defaults to `off`; no model-name guessing is used.
- `mode = "manual"` sends `thinking: { type: "enabled", budget_tokens }` and `temperature = 1`.
- `mode = "adaptive"` sends `thinking: { type: "adaptive", display }`, optional `output_config.effort`, and no temperature.
- `[skills]` is parsed and preserved as config only. It is intentionally not used to modify the system prompt yet.

## 4. Initial Skills Design

The first Skills implementation should stay simple:

```text
skills/
  rust.md
  finance.md
```

Each enabled skill loads a Markdown file by name from the configured paths and appends it to the system prompt in a stable wrapper:

```text
<skill name="rust">
...markdown...
</skill>
```

No frontmatter, installation, remote fetching, per-turn routing, or dynamic selection in the first Skills slice. The only purpose is to let users compose reusable instruction blocks from local files.

## 5. Non-Goals

- Context compression / Phase 7 integration.
- Skill marketplace or package installation.
- Skill metadata/frontmatter.
- Automatic skill discovery from the current working directory.
- Provider-specific third-party endpoint detection beyond explicit config fields.
- Persisting config back to disk.
