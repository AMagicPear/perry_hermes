//! `hermes` — the Hermes CLI.
//!
//! Phase 0: print version and a brief description, then exit. The real
//! REPL lands in phase 4 once `hermes-loop`, `hermes-providers`, and
//! `hermes-tools` have something to compose.
//!
//! See `plans/rust-port-design.md` for the full roadmap.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "hermes",
    version,
    about = "Hermes — a Rust port of the Hermes agent loop",
    long_about = None
)]
struct Args {
    /// No-op flag. Reserved for forward compatibility.
    #[arg(long, default_value_t = false)]
    _placeholder: bool,
}

fn main() {
    let _args = Args::parse();
    println!("hermes v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Phase 0 skeleton — the REPL lands in phase 4.");
    println!("See plans/rust-port-design.md for the roadmap.");
}
