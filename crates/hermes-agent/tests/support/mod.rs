use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream;
use hermes_core::message::{Content, Message};
use hermes_core::provider::{
    Completion, CompletionDelta, CompletionStream, Provider, ToolCallDelta,
};
use hermes_core::registry::ToolSchema;
use hermes_core::ProviderError;
use tokio_util::sync::CancellationToken;

pub struct ScriptedProvider {
    script: Mutex<Vec<Vec<CompletionDelta>>>,
    #[allow(dead_code)]
    call_count: AtomicUsize,
}

impl ScriptedProvider {
    #[allow(dead_code)]
    pub fn new(script: Vec<Completion>) -> Self {
        let script = script.into_iter().map(completion_to_deltas).collect();
        Self::from_deltas(script)
    }

    pub fn from_deltas(script: Vec<Vec<CompletionDelta>>) -> Self {
        Self {
            script: Mutex::new(script),
            call_count: AtomicUsize::new(0),
        }
    }
}

#[allow(dead_code)]
fn completion_to_deltas(c: Completion) -> Vec<CompletionDelta> {
    let mut deltas = Vec::new();
    let has_text = matches!(&c.message.content, Content::Text(t) if !t.is_empty());
    let has_reasoning = c.message.reasoning.as_ref().is_some_and(|s| !s.is_empty());

    if has_text || has_reasoning {
        deltas.push(CompletionDelta {
            content_delta: match &c.message.content {
                Content::Text(t) => Some(t.clone()),
                Content::Parts(_) => None,
            },
            reasoning_delta: c.message.reasoning.clone(),
            tool_call_delta: None,
            usage: Some(c.usage),
            finish_reason: None,
        });
    }

    if let Some(calls) = &c.message.tool_calls {
        deltas.extend(calls.iter().enumerate().map(|(index, tc)| CompletionDelta {
            content_delta: None,
            reasoning_delta: None,
            tool_call_delta: Some(ToolCallDelta {
                index,
                id: Some(tc.id.clone()),
                name: Some(tc.name.clone()),
                arguments_delta: Some(tc.arguments.to_string()),
            }),
            usage: None,
            finish_reason: None,
        }));
    }

    deltas.push(CompletionDelta {
        content_delta: None,
        reasoning_delta: None,
        tool_call_delta: None,
        usage: Some(c.usage),
        finish_reason: Some(c.finish_reason),
    });
    deltas
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            panic!(
                "ScriptedProvider: script exhausted - the loop called stream() more times than scripted"
            );
        }
        let deltas = script.remove(0);
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(deltas.into_iter().map(Ok))))
    }
}
