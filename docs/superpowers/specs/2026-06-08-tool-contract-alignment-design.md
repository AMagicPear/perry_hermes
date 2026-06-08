# Tool Contract Alignment Design

## Goal

Align Perry Hermes's model-visible tool surface with
`~/.hermes/hermes-agent` before adding deeper tool behavior. The model should
see the same names, parameter names, defaults, enum values, and usage guidance
for shared tools whenever the Rust runtime implements the same capability.

## Scope

This first alignment pass covers the foundational tools used for code work:

- `terminal`
- `read_file`
- `write_file`
- `patch`
- `search_files`
- `skills_list`
- `skill_view`

The pass also reserves the same names and toolsets for later high-leverage
tools: `todo`, `vision_analyze`, `session_search`, and `delegate_task`.

## Contract Rules

1. Python hermes-agent is the compatibility reference for model-visible
   contracts.
2. Tool names, toolsets, parameter names, defaults, enums, and descriptions
   should match the reference unless Rust intentionally does less.
3. If Rust intentionally does less, the tool should keep the compatible schema
   only when unsupported fields fail clearly at runtime.
4. New tools should use reference names. In particular:
   - targeted edits are `patch`, not `apply_patch`
   - search is `search_files`, not `search`
   - delegation toolset is `delegation`
5. Return payloads should be JSON when the reference returns JSON, with
   reference-compatible fields favored over Rust-only fields.

## Current Findings

`terminal` already uses the reference name and parameters. Rust currently
rejects `background`, `pty`, `notify_on_complete`, and `watch_patterns`, so
those fields remain compatibility surface rather than implemented behavior.

`read_file` already matches the reference name, parameters, and description.
Rust has basic pagination, blocked device reads, binary-extension rejection,
not-found suggestions, and a character cap. It does not yet implement read
deduplication, repeated-read loop blocking, secret redaction, or internal
Hermes read guards.

`write_file` already matches the reference name and parameters, including
`cross_profile`. Rust should add `files_modified` on success so callers can
handle write results the same way they do in hermes-agent.

`patch` is missing. It must expose the reference schema:
`mode`, `path`, `old_string`, `new_string`, `replace_all`, `patch`,
`cross_profile`. `mode="replace"` performs targeted find-and-replace.
`mode="patch"` accepts V4A patches.

`search_files` is missing. It must expose the reference schema:
`pattern`, `target`, `path`, `file_glob`, `limit`, `offset`, `output_mode`,
`context`. `target` is `content|files`; there is no `both` mode in the
reference contract.

`skills_list` and `skill_view` are already close to the reference. Rust's
schemas are slightly stricter because they set `additionalProperties: false`;
that is acceptable for now because the accepted parameters match.

## Implementation Strategy

Use a focused first implementation pass:

1. Add default metadata methods to `Tool` and expose them through
   `ToolSchema`: `toolset`, `is_async`, `requires_env`,
   `max_result_size_chars`, `emoji`, `available`.
2. Add `patch` using the reference schema. Implement both replace mode and a
   small V4A parser/application path.
3. Add `search_files` using the reference schema. Prefer `rg` when available
   and fall back to filesystem traversal for file search only if needed.
4. Register both tools under the existing `file` toolset.
5. Keep inventory/self-registration out of this pass; the current manual
   registry is small and easier to audit while the contract is being aligned.

## Non-Goals

- Implementing background terminal sessions.
- Implementing file backend abstraction for Docker/SSH.
- Implementing `todo`, `delegate_task`, `vision_analyze`, or
  `session_search`.
- Replacing JSON session storage with SQLite/FTS.
- Matching every hermes-agent tool.

## Acceptance

- `build_registry(&[], skills_dir)` exposes `terminal`, `read_file`,
  `write_file`, `patch`, `search_files`, `skills_list`, and `skill_view`.
- `patch` schema and `search_files` schema use the reference names and
  parameter shapes.
- `patch` replace mode edits a unique match, rejects non-unique matches unless
  `replace_all=true`, and reports `files_modified`.
- `patch` V4A mode can add, update, delete, and move files.
- `search_files` content mode returns line-oriented matches with pagination.
- `search_files` files mode returns file paths sorted by modification time.
- Existing `read_file`, `write_file`, `skills_list`, `skill_view`, and
  `terminal` tests stay green.
