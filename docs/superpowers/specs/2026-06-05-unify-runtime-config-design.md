# Unify Runtime Config — Design

**Date:** 2026-06-05
**Status:** Proposed
**Supersedes:** §2 / §3 of `2026-06-05-phase-9-config-and-skills-design.md` (the env-fallback bits)

## 1. Goal

Collapse the two coexisting configuration types in `hermes-runtime`
(`HermesConfig` for TOML, `AgentOptions` for runtime knobs) into a single
`HermesConfig`. Reduce the `AIAgent` constructor zoo to two entry points.
Make the TOML config file the single source of truth — remove implicit
environment-variable fallbacks (e.g. `OPENAI_MODEL`, `ANTHROPIC_BASE_URL`)
from the runtime.

## 2. Final shape

### 2.1 `HermesConfig` (unchanged TOML fields, no longer paired with a second struct)

```rust
pub struct HermesConfig {
    pub provider: ProviderConfig,    // kind, api_key_env, model, base_url, api_key_header, thinking
    pub agent: AgentConfig,          // max_iterations, disabled_toolsets, system_prompt
    pub skills: SkillsConfig,        // enabled, paths — preserved, not yet consumed
}
```

`ProviderConfig`, `AgentConfig`, `SkillsConfig`, `ProviderKind`, `ThinkingConfig`,
`ThinkingMode` keep their existing shape from `config.rs`. `AgentOptions` is
deleted entirely.

### 2.2 `SessionContext` (new — per-run context, not configuration)

```rust
pub struct SessionContext {
    pub working_dir: PathBuf,
    pub session_id: String,
}
```

`working_dir` and `session_id` are not part of `HermesConfig` because they
are runtime/session knobs that change between REPL sessions, not static
configuration. They travel alongside the message list into `run_*`.

### 2.3 `AIAgent` public API

Two constructors:

```rust
impl AIAgent {
    /// Load-time entry: TOML → provider, then delegate to `new`.
    pub fn from_config(config: HermesConfig) -> anyhow::Result<Self>;

    /// Programmatic entry: caller-supplied provider + config.
    pub fn new(provider: impl Provider + 'static, config: HermesConfig) -> Self;
}
```

Two run methods, both take `&SessionContext`:

```rust
pub async fn run_turn(
    &self,
    user_text: &str,
    session: &SessionContext,
    cancel: CancellationToken,
    on_event: impl FnMut(LoopEvent) + Send,
) -> Result<RunResult, LoopError>;

pub async fn run_messages(
    &self,
    messages: Vec<Message>,
    session: &SessionContext,
    cancel: CancellationToken,
    on_event: impl FnMut(LoopEvent) + Send,
) -> Result<RunResult, LoopError>;
```

`AIAgent` no longer stores `working_dir` or `session_id`. It holds only
`AgentLoop`. `ToolContext` is built per-call inside `run_*` from the
incoming `SessionContext`.

**Deleted constructors:** `openai_compatible`, `anthropic`,
`anthropic_with_api_key_header`, `echo`. Callers (CLI, example) build the
provider themselves and use `AIAgent::new` (or `from_config` if they have
only a config in hand).

**Deleted free function:** `merge_agent_options` — there is nothing left
to merge.

### 2.4 Environment variables

- `api_key_env` (in `[provider]`) **stays**. It is *explicit* configuration:
  the user writes `api_key_env = "ANTHROPIC_API_KEY"` in the TOML and the
  runtime reads that env var. This is not a fallback — it is a contract.
- Implicit fallbacks **removed**. The runtime no longer reads `OPENAI_MODEL`,
  `OPENAI_BASE_URL`, `ANTHROPIC_MODEL`, `ANTHROPIC_BASE_URL`, etc. as
  defaults. If the TOML omits `model` or `base_url`, `from_config` errors
  with a clear message naming the missing field.
- `api_key_header` keeps its hardcoded default of `"x-api-key"` for
  Anthropic (already in code) and stays `None` for OpenAI. Defaults here
  are OK because they are *type defaults*, not env-derived fallbacks.

## 3. CLI surface

### 3.1 New flags

```rust
struct Args {
    /// Path to HermesConfig TOML. If omitted, the CLI looks in
    /// `~/.perry_hermes/config.toml` then `./hermes.toml` (in that order).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Working directory for the session. Defaults to current dir.
    #[arg(long)]
    cwd: Option<PathBuf>,
}
```

That's it. `--provider`, `--model`, `--base-url`, `--max-iterations`,
`--disabled-toolsets` are **removed**. The TOML file owns those values.
`apply_cli_overrides` is deleted with them.

### 3.2 Config lookup order

The CLI resolves a config path in this order and stops at the first hit:

1. `--config <path>` if the user passed it (must exist; error if it doesn't).
2. `$HOME/.perry_hermes/config.toml` if it exists.
3. `./hermes.toml` (relative to the process's current working directory) if it exists.

If none of the three resolve to an existing file, the CLI errors out with
a clear message naming all three looked-up paths so the user can fix
their setup.

Implementation lives in `crates/hermes-cli/src/main.rs` as a small
`resolve_config_path` helper. The runtime's `HermesConfig::from_path`
remains the single TOML-loading primitive and does not know about the
default paths — keeping the runtime a pure data layer and leaving path
orchestration to the caller that cares about it.

### 3.3 Dispatch shape

```rust
async fn dispatch(args: Args) -> anyhow::Result<()> {
    let config_path = resolve_config_path(args.config.as_deref())?;
    let config = HermesConfig::from_path(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    let session = SessionContext {
        working_dir: args.cwd.unwrap_or_else(|| std::env::current_dir()?),
        session_id: "cli".into(),
    };

    let agent = AIAgent::from_config(config)?;
    run_repl(agent, &session).await
}
```

`run_repl(agent, &session)` constructs the per-turn `CancellationToken`
exactly as today, but passes `&session` to `run_messages`.

### 3.4 Missing-config error

When `--config` is omitted and neither default path exists, the error
message looks like:

```
error: no hermes config found. Looked for:
  - /Users/<user>/.perry_hermes/config.toml
  - <cwd>/hermes.toml
Pass --config <path> or create one of these. See
crates/hermes-cli/hermes.example.toml for a starter.
```

The example TOML lives at `crates/hermes-cli/hermes.example.toml`
(added as part of this work) so the path to a working config is one
`cp` away.

## 4. Example

`crates/hermes-runtime/examples/live_tool_use.rs` already does its own env
reading; it just needs to be rewired:

```rust
let api_key = std::env::var("OPENAI_API_KEY")?;
let base_url = std::env::var("OPENAI_BASE_URL")
    .unwrap_or_else(|_| "https://api.openai.com/v1".into());
let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

let provider = OpenAiProvider::new(&api_key, &model).with_base_url(&base_url);
let config = HermesConfig::default();
let session = SessionContext {
    working_dir: std::env::current_dir()?,
    session_id: "smoke".into(),
};
let agent = AIAgent::new(provider, config);
agent.run_turn(&user_text, &session, cancel, |event| { ... }).await
```

The env-var reads in the example are *caller-side* — the runtime itself
never reaches into the environment for defaults. To support
`HermesConfig::default()` we add `#[derive(Default)]` to `ProviderConfig`
with `ProviderKind::Echo` as the default `kind`; the field is unused when
the caller uses `AIAgent::new` with a hand-built provider.

## 5. Affected files

| File | Change |
|---|---|
| `crates/hermes-runtime/src/lib.rs` | Delete `AgentOptions`, `merge_agent_options`, four provider constructors. Add `SessionContext`. Refactor `from_config` / `new` to take `HermesConfig` only. Update `run_*` signatures. Move `tool_context` to take `&SessionContext`. |
| `crates/hermes-runtime/src/config.rs` | Add `#[derive(Default)]` to `ProviderConfig` and `ProviderKind`. No change to `from_path` — the required-field check lives in `AIAgent::from_config` (see lib.rs row), not in the TOML parser. The example path (`AIAgent::new` with a hand-built provider) bypasses it. |
| `crates/hermes-cli/src/main.rs` | Delete the six field CLI args, `apply_cli_overrides`, the `match args.provider` block. Add `--config` (optional) and `--cwd`. Add `resolve_config_path` helper implementing §3.2. Simplify `dispatch`. Update `run_repl` to pass `&session`. |
| `crates/hermes-cli/hermes.example.toml` | **New.** Sample TOML the user can copy. |
| `crates/hermes-runtime/examples/live_tool_use.rs` | Rewire to `AIAgent::new(provider, config)` + `SessionContext`. |
| `CLAUDE.md` | Update the "Runtime + CLI" paragraph to reflect the two-constructor API and the default config lookup order. |
| `docs/superpowers/specs/2026-06-05-phase-9-config-and-skills-design.md` | Add a "Superseded by" note at the top pointing to this spec. |

## 6. Testing

- **Config parsing tests** (already in `config.rs`): keep verbatim.
  `HermesConfig` and its sub-structs keep the same TOML shape, so the
  two existing tests pass without modification.
- **New unit tests** in `crates/hermes-runtime/src/lib.rs`:
  - `from_config` succeeds for a valid `HermesConfig` with each provider kind.
  - `from_config` returns a clear error if `[provider].model` is missing.
  - `from_config` returns a clear error if `[provider].base_url` is missing.
  - `from_config` returns a clear error if the env var named by
    `api_key_env` is unset.
  - `AIAgent::new(provider, HermesConfig::default())` builds a usable
    agent and `run_turn` reaches the provider (asserted via a
    `ScriptedProvider` from `hermes-loop`).
  - `SessionContext` is plumbed into `ToolContext`: a mock `Tool` that
    reads `ctx.working_dir` and `ctx.session_id` sees the values from
    the `SessionContext` passed to `run_turn`, not anything stored on
    the agent.
- **CLI integration test** (new, `crates/hermes-cli/tests/cli_smoke.rs`):
  - `hermes --config <fixture>` (using a TOML with the `echo` provider)
    reads a single line from stdin and exits 0. The point is to prove
    the wire is connected, not to assert the echo output.
  - `hermes` (no `--config`) with no file at the default paths exits
    non-zero with the "no hermes config found" message naming all three
    looked-up paths.
  - `hermes` with only `./hermes.toml` present (in the test's tempdir)
    picks it up automatically. With only `~/.perry_hermes/config.toml`
    present (env `HOME` redirected to a tempdir) it picks that up
    instead. First-hit wins — fixture a file at both locations and
    assert `./hermes.toml` is the one used.

## 7. Out of scope

- Skills loading (Phase 9 deferred; `SkillsConfig` shape is preserved but
  the runtime still ignores it).
- Per-turn toolset reconfiguration (`Toolset` filtering is still applied
  once at agent construction; per-turn changes are a separate spec).
- Removing `HermesConfig` as a TOML-deserializable type (the file-on-disk
  story is exactly what makes it the single source of truth — we are
  not removing that).
- A separate `RuntimeConfig` / `AppConfig` layer; the unified
  `HermesConfig` *is* the runtime config.
