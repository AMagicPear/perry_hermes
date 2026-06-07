//! Live smoke test for `OpenAiProvider` against any OpenAI-compatible
//! endpoint.
//!
//! Defaults to OpenAI's public API. Override via env vars to point at
//! DeepSeek, Moonshot, Zhipu, Ollama, your own vllm — any service that
//! speaks the `POST /v1/chat/completions` protocol with `Bearer` auth.
//!
//! Usage:
//!
//! ```bash
//! # OpenAI (defaults work as-is if OPENAI_API_KEY is set)
//! OPENAI_API_KEY=sk-... cargo run -p perry-hermes-providers --example live_smoke
//!
//! # DeepSeek
//! OPENAI_API_KEY=sk-... \
//! OPENAI_BASE_URL=https://api.deepseek.com/v1 \
//! OPENAI_MODEL=deepseek-chat \
//! cargo run -p perry-hermes-providers --example live_smoke
//!
//! # Ollama (locally running)
//! OPENAI_API_KEY=ollama \
//! OPENAI_BASE_URL=http://localhost:11434/v1 \
//! OPENAI_MODEL=llama3.2 \
//! cargo run -p perry-hermes-providers --example live_smoke
//! ```
//!
//! Optional positional argument overrides the user message:
//!
//! ```bash
//! cargo run -p perry-hermes-providers --example live_smoke -- "explain monads in one sentence"
//! ```
//!
//! # Convenience: `direnv` + `.envrc`
//!
//! Drop a `.envrc` in the project root with the exports you want, and
//! `direnv` will load them automatically on `cd`. See the
//! "Convenience" section of `README.md` (or just create `.envrc` with
//! the exports below).
//!
//! ```sh
//! # /Users/amagicpear/projects/perry_hermes/.envrc
//! export OPENAI_API_KEY=sk-cp-ug52bgtH3E-...
//! export OPENAI_BASE_URL=https://api.minimaxi.com/v1
//! export OPENAI_MODEL=MiniMax-M3
//! ```
//!
//! Then just:
//!
//! ```bash
//! cargo run -p perry-hermes-providers --example live_smoke
//! cargo run -p perry-hermes-providers --example live_smoke -- "explain monads"
//! ```

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use perry_hermes_core::message::{Content, Message, Role};
use perry_hermes_core::provider::Provider;
use perry_hermes_providers::OpenAiProvider;

#[tokio::main]
async fn main() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        eprintln!("error: OPENAI_API_KEY is not set");
        eprintln!();
        eprintln!("either export it:");
        eprintln!("  export OPENAI_API_KEY=sk-...");
        eprintln!("or use direnv to auto-load a project-local .envrc:");
        eprintln!("  brew install direnv");
        eprintln!("  echo 'eval \"$(direnv hook zsh)\"' >> ~/.zshrc");
        eprintln!("  cat > .envrc <<'EOF'");
        eprintln!("  export OPENAI_API_KEY=sk-...");
        eprintln!("  export OPENAI_BASE_URL=https://api.minimaxi.com/v1");
        eprintln!("  export OPENAI_MODEL=MiniMax-M3");
        eprintln!("  EOF");
        eprintln!("  direnv allow");
        std::process::exit(2);
    };

    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

    let user_text: String = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "say hi in one short sentence".into());

    eprintln!("→ POST {base_url}/chat/completions (model={model})");
    eprintln!("→ user: {user_text}");
    eprintln!();

    let provider = OpenAiProvider::new(&api_key, &model).with_base_url(&base_url);
    let cancel = CancellationToken::new();

    let user_msg = Message {
        role: Role::User,
        content: Content::Text(user_text),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    };

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        provider.complete(&[user_msg], &[], cancel),
    )
    .await;

    match result {
        Err(_) => {
            eprintln!("error: request timed out after 120s");
            std::process::exit(1);
        }
        Ok(Err(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Ok(Ok(completion)) => {
            let text = match &completion.message.content {
                Content::Text(s) => s.clone(),
                Content::Parts(_) => "<multimodal content>".into(),
            };
            println!("{text}");
            eprintln!();
            eprintln!(
                "← finish_reason={:?}  in={}  out={}  elapsed={:?}",
                completion.finish_reason,
                completion.usage.input_tokens,
                completion.usage.output_tokens,
                started.elapsed()
            );
        }
    }
}
