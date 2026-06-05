# Phase 9 — Skills Loading Design

**Date:** 2026-06-05
**Status:** Proposed
**Predecessor:** `2026-06-05-phase-9-config-and-skills-design.md` (proposed the config surface; this spec designs the loading + injection side)
**Supersedes:** the "Skills runtime loading deferred" wording in the predecessor doc and the placeholder `[skills]` schema in `HermesConfig`

## 1. Goal

Load skill files from a fixed global directory (`~/.perry_hermes/skills/`) and inject a **name + description index** into the system prompt at runtime. The skill bodies remain on disk and are loaded on demand in a future phase (Phase 12, paired with a `SkillActivationTool`); this spec delivers Level 1 metadata only, in line with the [Anthropic Agent Skills spec](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview).

The `HermesConfig.skills` placeholder and `SkillsConfig` struct are removed; the skills path is hard-coded in `hermes-runtime` and exposed as a single constant.

## 2. Non-Goals

- Skill body loading / `SkillActivationTool` (Phase 12)
- `frontmatter.platforms` filtering, conditional activation (`fallback_for` / `requires`)
- Skill marketplace, packaging, installation
- Per-turn reactive skill selection
- Skill mutation, curation, provenance tracking
- Cache invalidation via mtime/size snapshots (skills are loaded fresh per `AIAgent::from_config`; agents are typically per-session)
- Skill configuration variables (Phase 12)
- Cross-platform skill discovery (no `HERMES_PLATFORM` analog)

## 3. Bug Fix: Default System Prompt

In commit `5a7e141 refactor(runtime): unify AIAgent API on HermesConfig + SessionContext`, the `AgentOptions::default()` implementation that always set `system_prompt: Some(DEFAULT_SYSTEM_PROMPT.into())` was removed. From that point on, `config.agent.system_prompt = None` produced a `LoopConfig` with `system_prompt: None`, so the loop never injected a system message at all. `DEFAULT_SYSTEM_PROMPT` became dead code (verified by `rg DEFAULT_SYSTEM_PROMPT` returning only its definition site).

This spec revives `DEFAULT_SYSTEM_PROMPT` as the fallback when `config.agent.system_prompt` is `None`. The pre-unify default prompt is preserved verbatim:

> "You are a careful assistant with access to a `bash` tool. Use it to inspect the system or run shell commands when needed. When you have enough information to answer, give a concise final response — do not call tools again."

## 4. Architecture

New crate `crates/hermes-skills/`, depending only on `hermes-core` (for shared types if needed) and standard ecosystem crates. Dependency direction is strictly downward — `hermes-skills` is a leaf alongside `hermes-providers` and `hermes-tools`.

```
hermes-cli
  └─ hermes-runtime
       ├─ hermes-loop ─┐
       │               ├─ hermes-core
       └─ hermes-skills ┘
```

`hermes-skills` exposes two public functions:

```rust
/// Scan `skills_dir` for valid skills. Per-file errors (bad YAML, IO,
/// validation failures) are logged via `tracing::warn!` and the file is
/// skipped; only directory-level IO failures return `Err`.
pub fn load_all(skills_dir: &Path) -> anyhow::Result<Vec<Skill>>;

/// Render the system-prompt index block for the given skills.
/// Returns `""` when `skills` is empty.
pub fn render_system_prompt_block(skills: &[Skill]) -> String;
```

`hermes-runtime` calls both during `build_loop` (which is shared by `AIAgent::from_config` and `AIAgent::new`), so neither constructor needs to know about skills.

## 5. Data Shape

### 5.1 Directory Layout

A fixed global directory holds skill trees. By default this is `~/.perry_hermes/skills/`, computed from the `HOME` environment variable (matching the convention already used by the CLI for `~/.perry_hermes/config.toml`).

```
~/.perry_hermes/skills/
├── rust-core-style/              # top-level skill
│   └── SKILL.md
├── software-engineering/         # category container (exactly one level)
│   ├── rust-core-style/
│   │   └── SKILL.md
│   └── dogfood/
│       └── SKILL.md
└── ...
```

Path depth rules (strict):

| Path under `skills_dir` | Treated as |
|---|---|
| `<name>/SKILL.md` | Top-level skill, `category = None`, `qualified_name = <name>` |
| `<category>/<name>/SKILL.md` | Nested skill, `category = Some(<category>)`, `qualified_name = "<category>.<name>"` |
| `SKILL.md` directly under root | **Skipped** (warn) |
| `<a>/<b>/<c>/SKILL.md` (depth > 2) | **Skipped** (warn) |
| `something/without/SKILL.md` (other shape) | **Skipped** (warn) |

Excluded directory names (skip the entire subtree with one warn per hit), mirroring Python Hermes's `EXCLUDED_SKILL_DIRS`:

```
.git  .github  .hub  .archive  .venv  venv  node_modules
site-packages  __pycache__  .tox  .nox  .pytest_cache
.mypy_cache  .ruff_cache
```

### 5.2 `SKILL.md` Format

Frontmatter is required. **Leading blank lines before the first `---` are tolerated**; the frontmatter fence is the first non-empty line in the file. The closing fence is a standalone `---` followed by a newline. The body is everything after the closing fence (may be empty).

```markdown
---
name: rust-core-style
description: "Rust coding style and project conventions"
---

# Rust Style Guide

<markdown body...>
```

### 5.3 `name` Validation

Matches [Anthropic's official constraints](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview):

- Length ≤ 64 characters
- Only lowercase letters `a-z`, digits `0-9`, and hyphens `-`
- No `<` or `>` characters anywhere in the string (any literal angle bracket is rejected; this is simpler and stricter than a regex-based tag detector and matches the spirit of the Anthropic "no XML tags" rule)
- Not in the reserved set: `anthropic`, `claude`
- Directory name must equal `name` byte-for-byte

### 5.4 `description` Validation

- Non-empty
- Length ≤ 1024 characters
- No `<` or `>` characters (same rule as `name`)

### 5.5 `category` Validation

Same character set and length as `name` (lowercase letters, digits, hyphens, ≤ 64 chars). Reserved words also forbidden. A skill with a category-name validation failure skips the **entire** category subtree.

### 5.6 Other Frontmatter Fields

Preserved verbatim into `Skill.frontmatter: serde_yaml::Value`. The runtime does not interpret `version`, `platforms`, `prerequisites`, or `metadata`; they are available for future phases (notably Phase 12 curator and SkillActivationTool) without re-parsing the file.

### 5.7 `Skill` Struct

```rust
pub struct Skill {
    /// frontmatter.name, must equal the directory's basename
    pub name: String,
    /// "<category>.<name>", or just <name> when category is None
    pub qualified_name: String,
    /// Some("software-engineering") or None
    pub category: Option<String>,
    /// frontmatter.description (already validated)
    pub description: String,
    /// markdown body with the frontmatter block stripped
    pub body: String,
    /// full frontmatter as YAML value (other fields preserved)
    pub frontmatter: serde_yaml::Value,
    /// absolute path of the SKILL.md file (for logging and future use)
    pub source_path: PathBuf,
}
```

## 6. Load and Inject Flow

### 6.1 Loading (`hermes-skills`)

`load_all(skills_dir)`:

1. If `skills_dir` does not exist, return `Ok(vec![])`. The runtime emits a one-line info message to stderr ("no skills directory at …, skipping").
2. Read the directory. If `read_dir` itself fails, return `Err(anyhow!(…))` — caller downgrades this to a warn + empty list (see §7).
3. For each entry: classify by depth, recurse one level into category containers, treat two-level entries as nested skills, skip everything else.
4. For each candidate `SKILL.md`:
   - Read the file. On IO/encoding error, `tracing::warn!` (include `source_path`) and skip.
   - Strip frontmatter via `serde_yaml::from_str::<serde_yaml::Value>(...)` on the `---`-bounded block. If frontmatter is missing, malformed, or the YAML doesn't deserialize to a mapping, warn + skip.
   - Validate `name`, `description`, category. On any failure, warn + skip.
   - Check the directory basename matches `name`; if not, warn + skip.
   - Check for duplicate `qualified_name` within the same load; if duplicated, warn + skip the second one (keep the first found).
5. Sort by `(category, name)`. `None` category sorts after `Some(_)` categories (alphabetical), and within each group by `name` (alphabetical). This gives stable, deterministic order to the rendered block.
6. Return `Ok(skills)`.

### 6.2 Rendering (`hermes-skills`)

`render_system_prompt_block(skills)`:

- Empty input → return `""`.
- Otherwise build:

```
The following skills are available. Each skill is a directory containing a SKILL.md file with detailed instructions.
Use the `skill_view` tool (or read the file directly with bash) to load a skill's body when it is relevant to the user's request.

Available skills:

**software-engineering**:
- **rust-core-style**: Rust coding style and project conventions
- **dogfood**: Exploratory QA of web apps: find bugs, evidence, reports

**general** (uncategorized):
- **bash-conventions**: Bash scripting style and project conventions
```

- Categories are sorted alphabetically; `None` category groups under the `general` label and appears last.
- The `general` section is omitted entirely when every loaded skill is categorized.
- The `skill_view` tool reference is a deliberate forward reference for Phase 12; it does not break anything today because the LLM can use bash to read the file when terminal toolset is enabled. No tool is actually registered yet.

### 6.3 Runtime Wiring (`hermes-runtime`)

Add two helpers in `crates/hermes-runtime/src/lib.rs`:

```rust
fn default_skills_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".perry_hermes").join("skills"))
}

fn compose_system_prompt(user_prompt: Option<&str>) -> Option<String> {
    let skills = match default_skills_dir() {
        Some(d) => match hermes_skills::load_all(&d) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "failed to scan skills dir {}: {e}",
                    d.display()
                );
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let skills_block = hermes_skills::render_system_prompt_block(&skills);

    // Fix the §3 regression: when the user does not supply a system
    // prompt, fall back to DEFAULT_SYSTEM_PROMPT.
    let base = user_prompt.unwrap_or(DEFAULT_SYSTEM_PROMPT);

    if skills_block.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}\n\n{skills_block}"))
    }
}
```

`build_loop` changes from:

```rust
fn build_loop(provider: Arc<dyn Provider>, config: &HermesConfig) -> AgentLoop {
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets));
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt: config.agent.system_prompt.clone(),
            ..Default::default()
        },
    )
}
```

to:

```rust
fn build_loop(provider: Arc<dyn Provider>, config: &HermesConfig) -> AgentLoop {
    let registry = Arc::new(build_registry(&config.agent.disabled_toolsets));
    let system_prompt = compose_system_prompt(&config.agent.system_prompt);
    AgentLoop::from_provider(
        provider,
        registry,
        LoopConfig {
            max_iterations: config.agent.max_iterations.unwrap_or(10),
            system_prompt,
            ..Default::default()
        },
    )
}
```

The loop's existing system-prompt injection logic in `crates/hermes-loop/src/agent.rs` (the "if no System role present, prepend" branch) is unchanged. We only feed it more content.

### 6.4 Per-Run Context

Skills are configuration-time data, not per-run state. `SessionContext` (working_dir, session_id) is unrelated to skill loading. `AIAgent::run_turn` and `AIAgent::run_messages` are unaffected by this change.

## 7. Error Handling

| Situation | Behavior |
|---|---|
| `~/.perry_hermes/skills/` does not exist | `load_all` returns `Ok(vec![])`; runtime prints one info line to stderr. |
| `HOME` not set | `default_skills_dir` returns `None`; `compose_system_prompt` uses `DEFAULT_SYSTEM_PROMPT` and no skills block. |
| `SKILL.md` IO error (permission, vanished, bad encoding) | `tracing::warn!` (must include `source_path`); skip. |
| Frontmatter missing / not a YAML mapping / bad YAML | `tracing::warn!`; skip. |
| `name` validation failure (length, charset, XML, reserved) | `tracing::warn!` with the failing rule named; skip. |
| `description` validation failure (empty, > 1024, XML) | `tracing::warn!`; skip. |
| Directory basename != frontmatter `name` | `tracing::warn!` naming both; skip. |
| Wrong path depth (root `SKILL.md` or depth > 2) | `tracing::warn!`; skip. |
| `category` validation failure | `tracing::warn!`; skip entire category subtree. |
| Duplicate `qualified_name` within same category (including the same path scanned twice) | `tracing::warn!` naming the winner's path; skip the loser. |
| Same `name` under different categories | Both load (different `qualified_name`). |
| `read_dir` of skills_dir fails (whole dir inaccessible) | `load_all` returns `Err`; `compose_system_prompt` downgrades to `tracing::warn!` + empty list. **Does not fail `AIAgent::from_config`.** |

Invariants:

1. `AIAgent::from_config` never fails because of skills problems; all skills errors are best-effort.
2. Every `tracing::warn!` includes the offending `source_path` so the user can find and fix the file.
3. When zero skills are loaded (regardless of reason), the runtime still works — it falls back to the user's `system_prompt` (or `DEFAULT_SYSTEM_PROMPT`).

## 8. Testing Strategy

`hermes-skills` is new code; every rule gets at least one unit test. `hermes-runtime` integration tests cover the wiring. `hermes-loop` is untouched and needs no new tests.

### 8.1 `hermes-skills` Unit Tests (in-crate `#[cfg(test)] mod tests`)

Use `tempfile::tempdir()` to build a fresh skills tree per test.

Frontmatter parsing:
- `parses_skill_md_with_frontmatter_and_body` — happy path
- `returns_empty_body_when_only_frontmatter_present`
- `warns_and_skips_when_frontmatter_missing` — pure markdown
- `warns_and_skips_on_unterminated_frontmatter` — no closing `---`
- `warns_and_skips_on_invalid_yaml`

`name` validation:
- `accepts_valid_name_with_lowercase_letters_digits_hyphens`
- `rejects_name_with_uppercase_letters` → warn + skip
- `rejects_name_with_underscore`
- `rejects_name_longer_than_64_chars`
- `rejects_name_with_xml_tags`
- `rejects_reserved_name_anthropic`
- `rejects_reserved_name_claude`

`description` validation:
- `accepts_description_within_1024_chars`
- `rejects_empty_description`
- `rejects_description_longer_than_1024_chars`
- `rejects_description_with_xml_tags`

Layout:
- `loads_top_level_skill`
- `loads_one_level_nested_skill` — `qualified_name` is `cat.name`
- `warns_and_skips_two_level_nested_skill`
- `warns_and_skips_skill_md_at_skills_dir_root`
- `warns_when_dir_name_does_not_match_frontmatter_name`
- `warns_and_skips_duplicate_name_within_same_category`
- `loads_same_name_in_different_categories` — both load with different `qualified_name`
- `warns_and_skips_invalid_category_name`
- `skips_excluded_hidden_directories` (`.git`, `.venv`, etc.)

IO / missing dir:
- `returns_empty_vec_when_skills_dir_does_not_exist`
- `returns_err_on_read_dir_permission_denied` (Unix `chmod 000`)
- `skips_individual_file_with_io_error` (e.g. unreadable file inside otherwise-readable dir)
- `returns_skills_in_sorted_order` (deterministic sort)

Rendering:
- `render_empty_block_for_empty_vec` → `""`
- `render_block_groups_by_category_alphabetical`
- `render_block_sorts_skills_within_category_alphabetically`
- `render_block_groups_uncategorized_under_general_label`
- `render_block_omits_general_section_when_all_categorized`
- `render_block_includes_forward_reference_to_skill_view_tool`

### 8.2 `hermes-runtime` Integration Tests (`crates/hermes-runtime/tests/skills_injection.rs`, new file)

- `runtime_injects_skills_index_into_system_prompt_when_skills_dir_present` — use `ScriptedProvider` to capture `messages[0]`, assert it contains "Available skills" and the loaded skill names
- `runtime_uses_default_system_prompt_when_config_omits_it_and_skills_dir_absent` — covers the §3 fix; the resulting system message equals `DEFAULT_SYSTEM_PROMPT` verbatim, with no skills block
- `runtime_appends_skills_block_after_user_supplied_system_prompt` — `system_prompt = "my custom prompt"` + a skill → resulting system message starts with the custom prompt and contains "Available skills"
- `runtime_does_not_fail_construction_when_skills_dir_has_parse_errors` — best-effort behavior
- `runtime_loads_skills_from_home_perry_hermes_skills_path` — set `HOME` to a temp dir, drop a skill in `<temp>/.perry_hermes/skills/<name>/SKILL.md`, assert it shows up
- `runtime_uses_default_system_prompt_when_home_is_unset` — `HOME=""` (or removed) and no skills → system message equals `DEFAULT_SYSTEM_PROMPT` verbatim; no skills block, no panic

### 8.3 Manual Smoke (Documented, Not in CI)

Documented in `README.md` "Quick start": copy `crates/hermes-runtime/skills-example/rust-core-style/` to `~/.perry_hermes/skills/`, run echo provider, observe the rendered system prompt contains "Available skills" + "rust-core-style".

### 8.4 What We Don't Test

- Concurrent loading (not a requirement)
- Files larger than 1 MB (YAGNI)
- Semantic meaning of `frontmatter.platforms` or other preserved fields (out of scope)

## 9. File Changes

### New

| Path | Purpose |
|---|---|
| `crates/hermes-skills/Cargo.toml` | Crate manifest; depends on `serde`, `serde_yaml`, `anyhow`, `tracing` |
| `crates/hermes-skills/src/lib.rs` | `Skill` struct, `load_all`, `render_system_prompt_block`, all unit tests |
| `crates/hermes-runtime/tests/skills_injection.rs` | §8.2 integration tests |
| `crates/hermes-runtime/skills-example/rust-core-style/SKILL.md` | Manual smoke test fixture |
| `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md` | This file |

### Modified

| Path | Change |
|---|---|
| `Cargo.toml` (workspace root) | Add `"crates/hermes-skills"` to `members` |
| `Cargo.lock` | New `serde_yaml` dependency (auto) |
| `crates/hermes-runtime/Cargo.toml` | Add `hermes-skills = { path = "../hermes-skills" }` |
| `crates/hermes-runtime/src/lib.rs` | Add `default_skills_dir` + `compose_system_prompt`; route `build_loop` through them; resurrect `DEFAULT_SYSTEM_PROMPT` as the `None` fallback |
| `crates/hermes-runtime/src/config.rs` | Remove `SkillsConfig` and `HermesConfig.skills`; update `parses_anthropic_provider_config` test to omit the skills section |
| `crates/hermes-cli/hermes.example.toml` | Remove `[skills]` section |
| `CLAUDE.md` | Architecture diagram adds `hermes-skills`; "Known Issues" removes "Skills 加载待实现"; "Architecture" section describes skill injection flow |
| `README.md` | Top progress line: Phase 9 Skills "✅"; drop `[skills]` from example; add a "Skills" feature bullet; architecture diagram updated; "Known Issues" updated |
| `plans/hermes-comparison.md` | Phase 9 status → ✅; remove Skills from P1 list if present |
| `plans/rust-port-design.md` | §9.3 references this spec for implementation details |

### Untouched

- `crates/hermes-loop/` — `agent.rs` system-prompt injection logic stays as-is
- `crates/hermes-core/` — no new types needed; `serde_yaml` is consumed in `hermes-skills`
- `crates/hermes-providers/` / `crates/hermes-tools/` — independent leaf crates

## 10. Known Limitations (deferred to Phase 12)

These are accepted gaps in this spec, captured so they don't slip through silently:

1. **`skill_view` tool doesn't exist yet.** The rendered block tells the LLM to "use the `skill_view` tool (or read the file directly with bash) to load a skill's body." The `skill_view` tool is not registered. The fallback — bash — only works when the `terminal` toolset is enabled. **When the user sets `disabled_toolsets = ["terminal"]` (a perfectly reasonable config), the LLM sees the system prompt reference a tool that isn't reachable.** This is acceptable because this spec delivers metadata only; the LLM still knows what skills exist and can ask the user to enable the terminal toolset, or the user can re-enable it. Phase 12 ships the actual `SkillActivationTool` and closes this gap.
2. **No mtime / content-hash invalidation.** Skills are re-read on every `AIAgent::from_config` call. Editing a skill mid-session has no effect until the next agent is constructed. This is consistent with the per-session model (the CLI constructs one agent at startup) but worth documenting for gateways that may construct agents more often.
3. **Body in memory.** `Skill.body` is a full `String` of the file. For tens of skills this is fine; for thousands of skills it would add up. The index-rendering path never touches `body`, so this is paid only at construction time. No change required for Phase 9; future phases can lazy-load on demand.

## 11. Open Questions

None. The plan to deliver Level 1 metadata only, in the simplest possible shape, leaves Phase 12 (curator + SkillActivationTool + body loading) as a clean follow-up with its own spec.
