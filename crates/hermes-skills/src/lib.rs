//! Skill loading and system-prompt injection for the Hermes agent.
//!
//! See `docs/superpowers/specs/2026-06-05-phase-9_skills-loading-design.md`
//! for the full design. The public API is two functions:
//!
//! - [`load_all`]: scan a skills directory and return valid skills.
//! - [`render_system_prompt_block`]: render the metadata index for injection.