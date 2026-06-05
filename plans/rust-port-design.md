# Hermes 的 Rust 移植 — 设计文档

> **状态:** 历史设计草稿。当前代码已经实现 Phase 0–6，并在后续简化中调整了部分 API：`Provider::stream()` 是唯一必需 provider 方法；`ToolRegistry` trait 已删除，只保留 `InMemoryRegistry`; `AgentLoop<P, R>` 已收缩为持有 `Arc<dyn Provider>` + `InMemoryRegistry` 的 `AgentLoop`; `hermes-runtime::AIAgent` 是 CLI 和未来 gateway 的共享入口。本文保留早期设计思路和路线图，不应当逐字当作当前 API 文档。
>
> **范围:** 核心的 agent 循环 + 最简可用的工具/CLI。Hermes 的完整功能(17 个消息平台、Honcho 用户建模、curator、ACP 适配器、FTS5 session 搜索、Web 仪表盘等)在 v0 明确**不在范围内** — 想法是花几个周末做出一个有趣、可用的 agent。

---

## 0. 背景

### 0.1 Hermes 是什么?

`hermes-agent` 是 Nous Research 出品的自我进化型 AI agent,使用 Python 编写(v0.15.1,约 20 万+ 行代码)。其核心是 `run_agent.py` 中基于 ReAct 风格的对话循环(约 4.7k 行的 `AIAgent` 类),由 `model_tools.py` / `toolsets.py` 中的工具注册表支撑,在 `agent/` 和 `tools/` 下分布着约 80 个 provider/工具实现。

在 Rust 移植中需要保留的最重要特性:

- **工具调用循环** — LLM 发出 `tool_calls`,我们执行它们,把结果反馈回去,重复这个过程。
- **Provider 抽象** — 同一套代码路径可以对接 OpenAI / Anthropic / Gemini / Bedrock 等。
- **中断与预算** — 循环可以被取消,并受到迭代次数、token 数量和挂钟时间的约束。
- **流式输出** — LLM 响应逐 token 流式输出给用户。
- **工具错误是可恢复的** — 它们变成 `role: tool` 消息,而不是让循环中止的异常。

### 0.2 为什么用 Rust?

三个原因,都与 Hermes "runs anywhere" 的定位一致:

1. **单一静态二进制** — 部署到 5 美元的 VPS、serverless 沙箱或树莓派上,只需要一个文件。无需 Python venv,无需处理 wheel 兼容性矩阵。
2. **可预测的资源使用** — 没有 GIL,没有 GC 暂停,内存在编译期就可知上限。对长期运行的 gateway 进程来说很重要。
3. **类型系统能捕获 Python 代码库中通过注释记录的许多"沉默 bug"** — 例如"reasoning content 不在 message 上但本应在"这类 bug 类别。

### 0.3 参考:IronClaw(以及为什么它的帮助有限)

`nearai/ironclaw` 是一个 Rust agent OS,表面上看起来很相似,但它**是 OpenClaw 的 Rust 移植,而不是 Hermes 的**。它的 `FEATURE_PARITY.md` 跟踪的是 IronClaw 与 OpenClaw 的对比,Hermes 只是作为 `migrate` 命令的导入源被顺带提及。Hermes 特有的东西(curator/学习循环、Honcho 用户建模、17 个消息平台、ACP 适配器、claw 迁移、skin 引擎)在 IronClaw 中都没有对应实现。因此,IronClaw 的源码作为 **crate 结构的蓝图** 是有用的 — 参见 `crates/ironclaw_engine`、`ironclaw_llm`、`ironclaw_dispatcher`、`ironclaw_skills`、`ironclaw_mcp` — 但**不能**作为功能对功能的参考。Hermes 约 40–50% 的功能有 IronClaw 覆盖;其余是全新领域。

---

## 1. 心智模型:agent 循环是一个状态机

先把工具放一边。Agent 循环是一个**有限状态机**:

```
        ┌──────────────────────────────────────┐
        │                                      │
        ▼                                      │
   [Idle] ──user msg──▶ [Thinking] ──tool_calls──▶ [Acting] ──all done──▶ [Responding] ──▶ [Idle]
                        │                          │                          │
                        │ no tool_call             │ tool error                │ streaming
                        ▼                          ▼                          ▼
                   [Responding]              [Observing]                 [Idle]
```

Hermes 的 Python 实现(`run_agent.py` 中的 `run_conversation()`)使用带 `if` 分支的 `while` 循环来编码这个状态机。在 Rust 中我们可以做得更好:用 `enum` 建模状态,让 `match` 强制我们处理每一个状态转移。这让"LLM 返回 `finish_reason: length` 怎么办?"或"工具调用超时怎么办?"这类问题不会被遗忘。

每次迭代循环做三件事:

1. **组装上下文** — 系统提示 + 历史消息 + 当前用户输入。
2. **调用 LLM** — 发送消息和工具 schema 的 POST 请求,解析响应。
3. **对响应做出反应**:
   - `finish_reason: tool_use` → 执行工具,把结果作为 `role: tool` 消息追加,回到第 1 步。
   - `finish_reason: stop` → 返回助手的文本。结束。
   - `finish_reason: length` → 返回一个(可能被截断的)答案,并附带警告事件。
   - `finish_reason: content_filter` → 以错误中止。

Hermes 做出的最重要且不显然的设计决策,以及我们必须保留的:**工具错误不会中止循环**。它们被格式化为文本,作为 `role: tool` 消息反馈给 LLM,这样模型就能看到出了什么问题,然后选择重试或转向。这就是让 agent 能对不稳定的工具(网络抖动、临时文件错误等)具有鲁棒性的原因。

---

## 2. 核心类型

位于 `hermes-core/src/`。无 async,无 IO,除 serde 外无依赖。

### 2.1 消息

```rust
// hermes-core/src/message.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Content,

    /// 一些 provider(Anthropic extended thinking、OpenAI o1)把推理内容放在这里。
    /// 可选,这样普通消息就不用承担这个开销。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,

    /// 工具调用 id 的往返传输 — OpenAI 用 `tool_call_id`,Anthropic 用
    /// `tool_use_id`。在 provider 适配层做归一化。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role { System, User, Assistant, Tool }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),  // 多模态:文本 + 图像 + ...
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { url: String },
    // 未来:Audio, File, ...
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 原始 JSON。不要在 core 层解析成强类型结构 —
    /// 解析是每个工具自己的责任,因为 schema 是按工具定义的。
    pub arguments: serde_json::Value,
}
```

**设计决策:**

- `reasoning` 放在 **message 上**,而不是单独的字段。Hermes 中有专门注释提醒不要犯这个错 — reasoning 是助手消息的一部分,必须在压缩、序列化等过程中随消息一起传递。
- `Content` 是 `untagged` enum,这样同一个字段可以同时接受 `"hello"`(字符串)和 `[{"type": "text", ...}, ...]`(多模态数组)。LLM API 两者都接受。
- `ToolCall.arguments` 是 `serde_json::Value`,不是强类型结构。这里做强类型会迫使 core 层了解每个工具的 schema;schema 是按工具定义的,在工具派发时验证。

### 2.2 错误

```rust
// hermes-core/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("rate limited (retry after {retry_after_secs}s)")]
    RateLimited { retry_after_secs: u64 },
    #[error("context length exceeded: {0} tokens")]
    ContextLengthExceeded(u64),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timeout after {0}s")]
    Timeout(u64),
}

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("max iterations ({0}) reached")]
    MaxIterations(u32),
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
    #[error("cancelled")]
    Cancelled,
    #[error("content filter triggered")]
    ContentFilter,
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("context compression failed: {0}")]
    Compression(String),
}
```

**规则:** 库 crate 用 `thiserror`(类型化、可匹配),二进制 crate(CLI、gateway)只用 `anyhow`。不要混用。

### 2.3 Token 使用情况

```rust
// hermes-core/src/usage.rs
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 缓存的输入 token(Anthropic prompt caching、OpenAI cached)。用于成本报告。
    pub cached_input_tokens: u64,
}
```

---

## 3. Trait 边界:三个核心抽象

整个架构围绕 **三个 trait** 旋转。其他一切都是实现细节。

### 3.1 `Provider` — 与 LLM 通信

```rust
// hermes-core/src/provider.rs
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use crate::{Message, ToolSchema};
use crate::error::ProviderError;
use crate::usage::Usage;

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;

    /// 发送消息,获取一次完整响应。
    ///
    /// `tools` 是描述可用工具的 JSON Schema 对象列表。
    /// `cancel` 是循环用来表示"停止,用户按了 Ctrl-C"的 token。
    /// 实现必须 select 监听 `cancel` 并干净地退出。
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError>;

    /// 流式变体。默认实现可以回退到 `complete()` 并 yield 单个 chunk —
    /// 支持流式的 provider 覆盖此方法即可。
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<CompletionStream, ProviderError> {
        let _ = (messages, tools, &cancel);
        Err(ProviderError::Other("streaming not implemented".into()))
    }
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,           // 正常完成
    ToolUse,        // LLM 想调用工具
    Length,         // 触达 max_tokens
    ContentFilter,  // provider 阻止
    Error,          // provider 自己的错误
}

pub type CompletionStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = CompletionDelta> + Send>>;

#[derive(Debug, Clone)]
pub struct CompletionDelta {
    pub content_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    pub tool_call_delta: Option<ToolCall>,
    pub finish_reason: Option<FinishReason>,
}
```

### 3.2 `Tool` — LLM 可以请求的某个动作

```rust
// hermes-core/src/tool.rs
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;  // JSON Schema (draft-07)

    async fn execute(
        &self,
        args: Value,
        ctx: ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub working_dir: PathBuf,
    /// 按工具的权限策略。注册表解析这个;工具通过这个字段获得答案,
    /// 而不是自己检查配置。
    pub permissions: ToolPermissions,
}

#[derive(Debug, Clone, Default)]
pub struct ToolPermissions {
    pub network: bool,
    pub filesystem_write: bool,
    pub subprocess: bool,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// 作为 `role: tool` 消息反馈给 LLM 的文本。
    pub content: String,
    /// 可选附件(图像、文件) — v0 暂未支持。
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub data: Vec<u8>,
    pub mime: String,
}

#[derive(Debug, Clone, Copy)]
pub enum AttachmentKind { Image, File, Audio }
```

### 3.3 `ToolRegistry` — 发现和解析工具

```rust
// hermes-core/src/registry.rs
use std::collections::HashMap;
use std::sync::Arc;
use crate::tool::{Tool, ToolSchema};

pub trait ToolRegistry: Send + Sync {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
    fn names(&self) -> Vec<&str>;
    fn schemas(&self) -> Vec<ToolSchema>;
}

/// 默认的内存注册表。工具在启动时自行注册。
pub struct InMemoryRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl InMemoryRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }
    pub fn register(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }
}

impl ToolRegistry for InMemoryRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
    fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
    fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
```

**为什么 `CancellationToken` 要贯穿每个 async 方法**:在 Python 中,中断检查位于 `while` 循环的顶部,GIL 保证 Ctrl-C 信号最终会被送达。在 Rust async 中,对 LLM 的长 HTTP 请求不会自动响应 Ctrl-C,除非我们显式 select 监听 cancellation token。Hermes 的 `if self._interrupt_requested: break` 在 Python 中是意外能用的;在 Rust 中我们必须**有意地**接入它。

---

## 4. 循环

位于 `hermes-loop/src/agent.rs`。一旦类型和 trait 定义好,循环本身就很短。

```rust
// hermes-loop/src/agent.rs
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use hermes_core::error::LoopError;
use hermes_core::message::{Content, Message, Role, ToolCall};
use hermes_core::provider::{FinishReason, Provider};
use hermes_core::registry::ToolRegistry;
use hermes_core::tool::{ToolContext, ToolError, ToolOutput};

pub struct AgentLoop<P: Provider, R: ToolRegistry> {
    provider: P,
    registry: Arc<R>,
    config: LoopConfig,
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,        // Hermes 默认:90
    pub max_duration: Duration,     // 挂钟时间上限
    pub parallel_tool_calls: bool,  // 是否并行执行一批工具调用
    pub system_prompt: Option<String>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 90,
            max_duration: Duration::from_secs(60 * 10),
            parallel_tool_calls: false,
            system_prompt: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoopMetrics {
    pub iterations: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub final_message: Message,
    pub messages: Vec<Message>,
    pub metrics: LoopMetrics,
}

#[derive(Debug, Clone)]
pub enum LoopEvent {
    Thinking,
    AssistantMessage(Message),
    ToolCallStarted { call: ToolCall, iteration: u32 },
    ToolCallFinished { call: ToolCall, result: Result<ToolOutput, ToolError> },
    LengthLimit,
    IterationsExhausted,
    Cancelled,
}

impl<P: Provider, R: ToolRegistry> AgentLoop<P, R> {
    pub fn new(provider: P, registry: Arc<R>, config: LoopConfig) -> Self {
        Self { provider, registry, config }
    }

    /// 运行完整对话。在循环返回前可能发生多次工具调用迭代。
    ///
    /// 每个循环事件都会调用 `on_event` — CLI 用它做 spinner,
    /// gateway 用它把工具输出转发给用户,测试用它断言事件流。
    pub async fn run(
        &self,
        initial_messages: Vec<Message>,
        cancel: CancellationToken,
        mut on_event: impl FnMut(LoopEvent) + Send,
    ) -> Result<RunResult, LoopError> {
        let mut messages = initial_messages;
        let mut metrics = LoopMetrics::default();
        let started = Instant::now();

        // 如果配置了 system prompt,就在开头注入。
        if let Some(sys) = &self.config.system_prompt {
            if !messages.iter().any(|m| m.role == Role::System) {
                messages.insert(0, Message {
                    role: Role::System,
                    content: Content::Text(sys.clone()),
                    reasoning: None,
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
        }

        loop {
            // ── 1. 退出检查 ─────────────────────────────────────
            if cancel.is_cancelled() {
                on_event(LoopEvent::Cancelled);
                return Err(LoopError::Cancelled);
            }
            if metrics.iterations >= self.config.max_iterations {
                on_event(LoopEvent::IterationsExhausted);
                return Err(LoopError::MaxIterations(metrics.iterations));
            }
            if started.elapsed() > self.config.max_duration {
                return Err(LoopError::Timeout(started.elapsed()));
            }

            // ── 2. 解析工具 schema ────────────────────────────
            let tools = self.registry.schemas();

            // ── 3. 调用 LLM ────────────────────────────────────
            on_event(LoopEvent::Thinking);
            let completion = self.provider
                .complete(&messages, &tools, cancel.clone())
                .await?;
            metrics.iterations += 1;
            metrics.input_tokens += completion.usage.input_tokens;
            metrics.output_tokens += completion.usage.output_tokens;

            // ── 4. 持久化助手消息 ───────────────────────
            let assistant_msg = completion.message.clone();
            messages.push(assistant_msg.clone());
            on_event(LoopEvent::AssistantMessage(assistant_msg.clone()));

            // ── 5. 对 finish reason 做出反应 ──────────────────────
            match completion.finish_reason {
                FinishReason::Stop => {
                    metrics.duration = started.elapsed();
                    return Ok(RunResult {
                        final_message: assistant_msg,
                        messages,
                        metrics,
                    });
                }
                FinishReason::Length => {
                    on_event(LoopEvent::LengthLimit);
                    metrics.duration = started.elapsed();
                    return Ok(RunResult {
                        final_message: assistant_msg,
                        messages,
                        metrics,
                    });
                }
                FinishReason::ContentFilter => {
                    return Err(LoopError::ContentFilter);
                }
                FinishReason::Error => {
                    return Err(LoopError::Provider(
                        hermes_core::error::ProviderError::Other(
                            "provider returned finish_reason=error".into(),
                        ),
                    ));
                }
                FinishReason::ToolUse => {
                    let calls = assistant_msg
                        .tool_calls
                        .clone()
                        .unwrap_or_default();

                    if calls.is_empty() {
                        // provider 说 tool_use 但没有 tool_calls — 奇怪,
                        // 直接 bail 避免无限循环
                        return Err(LoopError::Provider(
                            hermes_core::error::ProviderError::InvalidResponse(
                                "finish_reason=tool_use but no tool_calls".into(),
                            ),
                        ));
                    }

                    for call in calls {
                        if cancel.is_cancelled() {
                            on_event(LoopEvent::Cancelled);
                            return Err(LoopError::Cancelled);
                        }
                        on_event(LoopEvent::ToolCallStarted {
                            call: call.clone(),
                            iteration: metrics.iterations,
                        });

                        let result = self.dispatch_tool(&call, cancel.clone()).await;

                        // 构造 tool-result 消息。**错误不是致命的** —
                        // 它们变成 role=tool 消息,这样 LLM 能看到错误并重试/转向。
                        let tool_msg = match &result {
                            Ok(out) => Message {
                                role: Role::Tool,
                                content: Content::Text(out.content.clone()),
                                reasoning: None,
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            },
                            Err(e) => Message {
                                role: Role::Tool,
                                content: Content::Text(format!(
                                    "Error: {e}"
                                )),
                                reasoning: None,
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            },
                        };
                        messages.push(tool_msg);
                        on_event(LoopEvent::ToolCallFinished {
                            call: call.clone(),
                            result,
                        });
                        metrics.tool_calls += 1;
                    }
                    // 循环继续:LLM 看到工具结果,决定下一步做什么。
                }
            }
        }
    }

    async fn dispatch_tool(
        &self,
        call: &ToolCall,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .registry
            .get(&call.name)
            .ok_or_else(|| ToolError::NotFound(call.name.clone()))?;

        // 在传入参数前,先用工具的 JSON Schema 验证 LLM 的参数。
        // LLM *会* 偶尔产出无效的 JSON。
        let args = validate_args(&call.arguments, tool.parameters_schema())
            .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

        let ctx = ToolContext {
            session_id: "default".to_string(),  // 在 runtime 层接入
            working_dir: std::env::current_dir().unwrap_or_default(),
            permissions: Default::default(),     // 同上
        };

        tool.execute(args, ctx, cancel).await
    }
}

/// 用 JSON Schema 验证 `args`。简单但能捕获"缺少必填字段"/
/// "类型错误"这类常见错误。
fn validate_args(
    args: &serde_json::Value,
    schema: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use jsonschema::JSONSchema;
    let compiled = JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .compile(&schema)
        .map_err(|e| format!("schema compile: {e}"))?;
    let result = compiled.validate(args);
    if let Err(errors) = result {
        let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
        return Err(msgs.join("; "));
    }
    Ok(args.clone())
}
```

**关键设计点:**

1. **`on_event: impl FnMut` 回调**是唯一的副作用通道。对于简单场景(单消费者、无背压),它比 `mpsc::Sender` 好,显然也比 `println!` 好。Hermes 中对应的是 `agent/display.py` 里的 `_KawaiiSpinner` + activity feed。

2. **返回 `RunResult`,而不是 `String`。** CLI 只需要 `result.final_message`。轨迹压缩器(Hermes 中的 `trajectory_compressor.py`)需要 `result.messages`。训练数据生成需要两者。别把信息扔掉。

3. **`cancel` 通过 `clone()` 传入 async 调用。** `CancellationToken` 是 Arc 包装的标志位 — clone 很便宜。

4. **工具错误不会用 `?` 向上传播。** 它们变成 `content: "Error: ..."` 的 `role: tool` 消息。这就是 agent 和 chatbot 的区别。一旦搞错,agent 会在第一次 `cat /etc/shadow` 权限错误时就死掉。

5. **验证发生在派发时,而不是 LLM 边界。** LLM 不懂 Rust 类型;它发的是原始 JSON。我们对工具的 JSON Schema 编译一次(可以缓存),在调用工具前验证参数。

---

## 5. Crate 结构

依赖方向**只向下**。没有反向边。

```
hermes-cli              (binary)
   │
hermes-runtime          (AIAgent facade — 用户面向的 API)
   │
   ├──> hermes-loop     (循环)
   │       │
   │       ├──> hermes-core           (类型、trait、错误)
   │       │
   │       ├──> hermes-providers      (OpenAI / Anthropic / ...)
   │       │       └──> hermes-core
   │       │
   │       └──> hermes-tools          (ToolRegistry + 内置工具)
   │               └──> hermes-core
   │
hermes-gateway          (Telegram / Discord / Slack)
   └──> hermes-runtime
   └──> hermes-core

hermes-tui              (ratatui)
   └──> hermes-runtime
```

**为什么这样布局:**

- `hermes-core` 没有依赖(除 serde/tokio-util 之外)→ 编译约 1 秒,可以从其他每个 crate 轻松 mock。
- `hermes-providers` 和 `hermes-tools` 都依赖 `core`,但**互不依赖**。添加新 provider 不会触碰 tools;添加新 tool 不会触碰 providers。两个开发者可以并行工作。
- `hermes-runtime` 是**产品 API**。用户写 `use hermes_runtime::AIAgent;`。他们看不到 `hermes-loop`。这就是 Hermes 中 `run_agent.py` 扮演的角色。
- 二进制 crate(`hermes-cli`、`hermes-gateway`、`hermes-tui`)相互独立 — 你可以 `cargo build -p hermes-cli` 不编译 gateway。

### Cargo workspace `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = [
    "crates/hermes-core",
    "crates/hermes-providers",
    "crates/hermes-tools",
    "crates/hermes-loop",
    "crates/hermes-runtime",
    "crates/hermes-cli",
    "crates/hermes-gateway",  # 后续
    "crates/hermes-tui",      # 后续
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[workspace.dependencies]
async-trait = "0.1"
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
futures = "0.3"
jsonschema = "0.18"
chrono = { version = "0.4", features = ["serde"] }
clap = { version = "4", features = ["derive"] }
reqwest = { version = "0.12", features = ["json", "stream"] }

# 内部
hermes-core = { path = "crates/hermes-core" }
hermes-providers = { path = "crates/hermes-providers" }
hermes-tools = { path = "crates/hermes-tools" }
hermes-loop = { path = "crates/hermes-loop" }
hermes-runtime = { path = "crates/hermes-runtime" }
```

---

## 6. 渐进式实现路线图

**不要从 8 个 crate 开始。** 每个阶段都要产出可运行的东西。

| 阶段 | 目标 | 关键文件 | LOC |
|------:|------|-----------|----:|
| **0. 骨架** | 可编译的空 workspace | 8 个 crate stub + trait | ~500 |
| **1. Echo 循环** | Mock provider 返回 `Stop`,循环跑一次,测试通过 | `crates/hermes-loop/src/agent.rs` + 测试 | ~300 |
| **2. OpenAI provider** | 真实 `gpt-4o-mini` 调用可用 | `crates/hermes-providers/src/openai.rs` | ~600 |
| **3. 一个真实工具** | `BashTool` 能跑 shell 命令 | `crates/hermes-tools/src/bash.rs` | ~400 |
| **4. CLI** ✅ | 输入消息,看到 agent 完成(交互式 REPL + clap + 多轮历史 + 事件渲染 + 工具集过滤) | `crates/hermes-cli/src/main.rs` | ~500 |
| **5. 流式** ✅ | token 到达时立即打印 | `Provider::stream` + SSE parser + `LoopEvent::ContentDelta` | +300 |
| **6. 中断** ✅ | Ctrl-C 在流式中途能停下并保留 partial assistant message | `CancellationToken` + `LoopError::CancelledWith` | +200 |
| **7. 上下文压缩** | 消息过长时做摘要 | `ContextCompressor` trait | ~500 |
| **8. Anthropic provider** | 体验多 provider 差异 | `crates/hermes-providers/src/anthropic.rs` | ~600 |
| **9. Skills** | 加载 `.md` 文件作为 system-prompt 内容 | skill scanner | ~400 |
| **10. TUI** | ratatui 多行编辑 | `crates/hermes-tui/` | ~2000 |
| **11. 第一个平台** | 通过 `grammY-rs` 接入 Telegram | `crates/hermes-gateway/` | ~1500 |
| **12. Curator** | 学习循环 — Hermes 的灵魂 | `crates/hermes-curator/` | ~2000 |

**到第 4 阶段结束你就拥有了一个有趣、可用的 agent。** 预算 ~1–2 个周末的零散时间就能走到那里。之后的工作是独立的 — 你可以挑任何感兴趣的方向。

---

## 7. 骨架代码(第 0–4 阶段)

下面的代码是从 `cargo new` 到"我能跑 `hermes-cli` 让它和真实 LLM 通信并执行真实工具"的最低要求。

### 7.1 `Cargo.toml`(workspace 根)

参见第 5 节。

### 7.2 `crates/hermes-core/Cargo.toml`

```toml
[package]
name = "hermes-core"
version.workspace = true
edition.workspace = true

[dependencies]
async-trait.workspace = true
tokio-util.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
futures.workspace = true
```

### 7.3 `crates/hermes-core/src/lib.rs`

```rust
pub mod error;
pub mod message;
pub mod provider;
pub mod registry;
pub mod tool;
pub mod usage;

pub use error::{LoopError, ProviderError, ToolError};
pub use message::{Content, ContentPart, Message, Role, ToolCall};
pub use provider::{Completion, CompletionDelta, CompletionStream, FinishReason, Provider};
pub use registry::{InMemoryRegistry, ToolRegistry, ToolSchema};
pub use tool::{Attachment, AttachmentKind, Tool, ToolContext, ToolOutput, ToolPermissions};
pub use usage::Usage;
```

### 7.4 `crates/hermes-core/src/message.rs`

参见第 2.1 节。

### 7.5 `crates/hermes-core/src/error.rs`

参见第 2.2 节。

### 7.6 `crates/hermes-core/src/provider.rs`

参见第 3.1 节。

### 7.7 `crates/hermes-core/src/tool.rs`

参见第 3.2 节。

### 7.8 `crates/hermes-core/src/registry.rs`

参见第 3.3 节。

### 7.9 `crates/hermes-core/src/usage.rs`

```rust
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}
```

### 7.10 `crates/hermes-providers/Cargo.toml`

```toml
[package]
name = "hermes-providers"
version.workspace = true
edition.workspace = true

[dependencies]
hermes-core.workspace = true
async-trait.workspace = true
tokio.workspace = true
tokio-util.workspace = true
serde.workspace = true
serde_json.workspace = true
reqwest.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

### 7.11 `crates/hermes-providers/src/lib.rs`

```rust
pub mod echo;
pub mod openai;

pub use echo::EchoProvider;
pub use openai::OpenAiProvider;
```

### 7.12 `crates/hermes-providers/src/echo.rs` — v0 provider

一个 mock provider,总是返回 `Stop`,并把用户的最后一条消息 echo 回来。让你可以在没有 API key 的情况下测试循环。

```rust
// crates/hermes-providers/src/echo.rs
use async_trait::async_trait;
use hermes_core::{
    message::{Content, Message, Role},
    provider::{Completion, FinishReason, Provider},
    ProviderError, ToolSchema, Usage,
};
use tokio_util::sync::CancellationToken;

pub struct EchoProvider {
    name: String,
    model: String,
}

impl EchoProvider {
    pub fn new() -> Self {
        Self { name: "echo".into(), model: "echo-v0".into() }
    }
}

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str { &self.name }
    fn model(&self) -> &str { &self.model }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        // Echo 最后一条用户消息。
        let last_user = messages.iter().rev()
            .find(|m| m.role == Role::User)
            .cloned()
            .unwrap_or(Message {
                role: Role::Assistant,
                content: Content::Text("(nothing to echo)".into()),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            });
        // 合成一个假的助手回复。
        let reply = match &last_user.content {
            Content::Text(t) => format!("echo: {t}"),
            Content::Parts(_) => "echo: (multimodal)".to_string(),
        };
        Ok(Completion {
            message: Message {
                role: Role::Assistant,
                content: Content::Text(reply),
                reasoning: None,
                tool_call_id: None,
                tool_calls: None,
            },
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    }
}
```

### 7.13 `crates/hermes-providers/src/openai.rs` — 真实 provider

一个最小的 OpenAI Chat Completions provider。流式留作"默认返回错误"的 stub;第 5 阶段再接入。

```rust
// crates/hermes-providers/src/openai.rs
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use hermes_core::{
    message::{Content, ContentPart, Message, Role, ToolCall},
    provider::{Completion, FinishReason, Provider},
    ProviderError, ToolSchema, Usage,
};

pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<OaiMessage<'a>>,
    tools: Vec<OaiTool<'a>>,
    tool_choice: &'static str,  // "auto"
}

#[derive(Serialize)]
struct OaiMessage<'a> {
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCallRef<'a>>>,
}

#[derive(Serialize)]
struct OaiToolCallRef<'a> {
    id: &'a str,
    r#type: &'static str,  // "function"
    function: OaiFunctionCallRef<'a>,
}

#[derive(Serialize)]
struct OaiFunctionCallRef<'a> {
    name: &'a str,
    arguments: &'a serde_json::Value,
}

#[derive(Serialize)]
struct OaiTool<'a> {
    r#type: &'static str,  // "function"
    function: OaiFunctionDef<'a>,
}

#[derive(Serialize)]
struct OaiFunctionDef<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<OaiUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: OaiRespMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiRespMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Deserialize)]
struct OaiToolCall {
    id: String,
    function: OaiFunctionCall,
}

#[derive(Deserialize)]
struct OaiFunctionCall {
    name: String,
    /// 用 String 是因为 OpenAI 流式/返回的是 JSON 字符串,
    /// 不是 JSON 对象。
    arguments: String,
}

#[derive(Deserialize)]
struct OaiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str { "openai" }
    fn model(&self) -> &str { &self.model }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        cancel: CancellationToken,
    ) -> Result<Completion, ProviderError> {
        // ── 把消息序列化到 OpenAI 格式 ──────────────
        let oai_msgs: Vec<OaiMessage> = messages.iter().map(|m| {
            let content = match &m.content {
                Content::Text(s) => Some(s.as_str()),
                Content::Parts(parts) => parts.iter().find_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                }),
            };
            let tool_calls = m.tool_calls.as_ref().map(|calls| {
                calls.iter().map(|c| OaiToolCallRef {
                    id: &c.id,
                    r#type: "function",
                    function: OaiFunctionCallRef {
                        name: &c.name,
                        arguments: &c.arguments,
                    },
                }).collect()
            });
            OaiMessage {
                role: match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                },
                content,
                tool_call_id: m.tool_call_id.as_deref(),
                tool_calls,
            }
        }).collect();

        let oai_tools: Vec<OaiTool> = tools.iter().map(|t| OaiTool {
            r#type: "function",
            function: OaiFunctionDef {
                name: &t.name,
                description: &t.description,
                parameters: &t.parameters,
            },
        }).collect();

        let req = ChatRequest {
            model: &self.model,
            messages: oai_msgs,
            tools: oai_tools,
            tool_choice: "auto",
        };

        // ── 发起请求,带 cancel 竞态 ─────────
        let url = format!("{}/chat/completions", self.base_url);
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = self.client.post(&url)
                .bearer_auth(&self.api_key)
                .json(&req)
                .send() => r.map_err(ProviderError::Transport)?,
        };

        if resp.status() == 401 {
            return Err(ProviderError::Auth(resp.text().await.unwrap_or_default()));
        }
        if resp.status() == 429 {
            // 简单实现:假定 1s 退避。真实实现应该读 `retry-after` header。
            return Err(ProviderError::RateLimited { retry_after_secs: 1 });
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::InvalidResponse(body));
        }

        let parsed: ChatResponse = resp.json()
            .await
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;

        let choice = parsed.choices.into_iter().next()
            .ok_or_else(|| ProviderError::InvalidResponse("no choices".into()))?;

        // ── 解析 finish_reason ──────────────────────────────
        let finish_reason = match choice.finish_reason.as_deref() {
            Some("stop") => FinishReason::Stop,
            Some("tool_calls") => FinishReason::ToolUse,
            Some("length") => FinishReason::Length,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        };

        // ── 解析 tool_calls(arguments 是 JSON 字符串) ──
        let tool_calls = choice.message.tool_calls.map(|calls| {
            calls.into_iter().map(|c| ToolCall {
                id: c.id,
                name: c.function.name,
                arguments: serde_json::from_str(&c.function.arguments)
                    .unwrap_or(serde_json::Value::Null),
            }).collect()
        });

        let usage = parsed.usage.map(|u| Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: 0,
        }).unwrap_or_default();

        Ok(Completion {
            message: Message {
                role: Role::Assistant,
                content: Content::Text(choice.message.content.unwrap_or_default()),
                reasoning: None,
                tool_call_id: None,
                tool_calls,
            },
            usage,
            finish_reason,
        })
    }
}
```

### 7.14 `crates/hermes-tools/Cargo.toml`

```toml
[package]
name = "hermes-tools"
version.workspace = true
edition.workspace = true

[dependencies]
hermes-core.workspace = true
async-trait.workspace = true
tokio.workspace = true
tokio-util.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

### 7.15 `crates/hermes-tools/src/lib.rs`

```rust
pub mod bash;

pub use bash::BashTool;
```

### 7.16 `crates/hermes-tools/src/bash.rs` — 最简工具

运行一条 shell 命令。**目前没有沙箱** — 第 12+ 阶段会把它迁到 IronClaw 的 `ironclaw_wasm` 风格沙箱里。在那之前,不要在你重视的机器上跑这个。

```rust
// crates/hermes-tools/src/bash.rs
use async_trait::async_trait;
use hermes_core::{
    tool::{Tool, ToolContext, ToolError, ToolOutput},
};
use serde_json::Value;
use std::process::Stdio;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub struct BashTool;

impl BashTool {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn description(&self) -> &str {
        "Run a shell command and return its combined stdout+stderr. \
         Use for file operations, running scripts, inspecting the system, etc."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Maximum wall-clock seconds before the command is killed.",
                    "default": 30,
                    "minimum": 1,
                    "maximum": 600
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        args: Value,
        ctx: ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let command = args.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'command'".into()))?;
        let timeout_secs = args.get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        // 用 shell 跑,这样用户可以 pipe / redirect / 用 `&&` 等。
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        // 竞态:进程退出 vs 取消 vs 超时。
        let timeout = tokio::time::Duration::from_secs(timeout_secs);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(ToolError::Cancelled);
            }
            _ = tokio::time::sleep(timeout) => {
                let _ = child.kill().await;
                return Err(ToolError::Timeout(timeout_secs));
            }
            status = child.wait() => {
                let status = status.map_err(|e| ToolError::Execution(e.to_string()))?;
                let mut out = String::new();
                if let Some(mut s) = child.stdout.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_string(&mut out).await;
                }
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_string(&mut err).await;
                }
                let combined = if err.is_empty() {
                    out
                } else if out.is_empty() {
                    err
                } else {
                    format!("{out}\n--- stderr ---\n{err}")
                };
                // 截断过大的输出 — Hermes 也是这么做的。
                let truncated = if combined.len() > 50_000 {
                    format!(
                        "{}\n... [truncated, full output {} bytes] ...",
                        &combined[..25_000],
                        combined.len()
                    )
                } else {
                    combined
                };
                let exit_note = if status.success() {
                    String::new()
                } else {
                    format!("\n[exit code {}]", status.code().unwrap_or(-1))
                };
                Ok(ToolOutput {
                    content: format!("{truncated}{exit_note}"),
                    attachments: vec![],
                })
            }
        }
    }
}
```

### 7.17 `crates/hermes-loop/Cargo.toml`

```toml
[package]
name = "hermes-loop"
version.workspace = true
edition.workspace = true

[dependencies]
hermes-core.workspace = true
async-trait.workspace = true
tokio.workspace = true
tokio-util.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tracing.workspace = true
jsonschema.workspace = true
```

### 7.18 `crates/hermes-loop/src/lib.rs`

```rust
pub mod agent;

pub use agent::{AgentLoop, LoopConfig, LoopEvent, LoopMetrics, RunResult};
```

### 7.19 `crates/hermes-loop/src/agent.rs`

完整文件见第 4 节。

### 7.20 `crates/hermes-loop/tests/echo_loop.rs` — 第 1 阶段冒烟测试

```rust
// crates/hermes-loop/tests/echo_loop.rs
use std::sync::Arc;
use hermes_core::message::{Content, Message, Role};
use hermes_core::registry::InMemoryRegistry;
use hermes_loop::{AgentLoop, LoopConfig};
use hermes_providers::EchoProvider;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn echo_provider_runs_one_iteration_and_stops() {
    let provider = EchoProvider::new();
    let registry = Arc::new(InMemoryRegistry::new());
    let loop_ = AgentLoop::new(provider, registry, LoopConfig {
        max_iterations: 5,
        ..Default::default()
    });

    let messages = vec![Message {
        role: Role::User,
        content: Content::Text("hello".into()),
        reasoning: None,
        tool_call_id: None,
        tool_calls: None,
    }];

    let cancel = CancellationToken::new();
    let events = std::sync::Mutex::new(Vec::new());

    let result = loop_.run(messages, cancel, |e| {
        events.lock().unwrap().push(format!("{e:?}"));
    }).await.unwrap();

    assert_eq!(result.metrics.iterations, 1);
    assert_eq!(result.metrics.tool_calls, 0);
    let final_text = match result.final_message.content {
        Content::Text(s) => s,
        _ => panic!("expected text"),
    };
    assert_eq!(final_text, "echo: hello");
}
```

### 7.21 `crates/hermes-cli/Cargo.toml`

```toml
[package]
name = "hermes-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "hermes"
path = "src/main.rs"

[dependencies]
hermes-core.workspace = true
hermes-providers.workspace = true
hermes-tools.workspace = true
hermes-loop.workspace = true
tokio.workspace = true
tokio-util.workspace = true
serde_json.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
clap.workspace = true
futures.workspace = true
```

### 7.22 `crates/hermes-cli/src/main.rs`

```rust
// crates/hermes-cli/src/main.rs
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use anyhow::Context;
use clap::Parser;
use hermes_core::message::{Content, Message, Role};
use hermes_core::registry::InMemoryRegistry;
use hermes_core::tool::Tool;
use hermes_loop::{AgentLoop, LoopConfig, LoopEvent};
use hermes_providers::{EchoProvider, OpenAiProvider};
use hermes_tools::BashTool;
use tokio_util::sync::CancellationToken;

#[derive(Parser, Debug)]
#[command(version, about = "Minimal Hermes-style agent in Rust")]
struct Args {
    /// LLM provider: "openai" or "echo"
    #[arg(long, default_value = "openai")]
    provider: String,

    /// Model name (ignored for echo)
    #[arg(long, default_value = "gpt-4o-mini")]
    model: String,

    /// System prompt
    #[arg(long, default_value = "You are a helpful assistant with a bash tool. Be concise.")]
    system: String,

    /// Max loop iterations
    #[arg(long, default_value_t = 20)]
    max_iterations: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hermes_loop=debug".into())
        )
        .init();

    let args = Args::parse();

    // ── Provider ────────────────────────────────────────────
    let provider: Box<dyn hermes_core::provider::Provider> = match args.provider.as_str() {
        "echo" => Box::new(EchoProvider::new()),
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY")
                .context("set OPENAI_API_KEY or use --provider echo")?;
            Box::new(OpenAiProvider::new(key, &args.model))
        }
        other => anyhow::bail!("unknown provider: {other}"),
    };

    // 等等 — 单个 field 不能用两个不同具体类型的 dyn dispatch。
    // 现在先挑一个具体 provider,在构造时分支。
    let _ = provider;  // 见下面的备选方案

    // ── 备选方案(更简单,不用 Box):一个 main 分支对应一个 provider ─
    // (取消你想用那个分支的注释;把上面的 Box 删掉。)
    //
    // let provider = EchoProvider::new();
    // let provider = OpenAiProvider::new(
    //     std::env::var("OPENAI_API_KEY")?,
    //     &args.model,
    // );

    // ↓↓↓ 下面的代码块假设有一个 `provider` 值在作用域内。
    // ↓↓↓ 用上面某个具体 provider 替换 `_ = provider;` 那行;
    // ↓↓↓ main 的其余部分照常工作。
    todo!("uncomment a concrete provider above; see comment")
}
```

> **关于 `Box<dyn Provider>` 问题的说明。** Rust 不允许你在不使用 enum 或 trait object 的情况下,把两个不同具体类型赋给同一个变量。最干净的修复方式是把 `main` 写成泛型函数,或者把 provider 装箱(`Box<dyn Provider>`)。下面是能跑通的版本:
>
> ```rust
> use hermes_core::provider::Provider;
>
> let provider: Arc<dyn Provider> = match args.provider.as_str() {
>     "echo" => Arc::new(EchoProvider::new()),
>     "openai" => {
>         let key = std::env::var("OPENAI_API_KEY")?;
>         Arc::new(OpenAiProvider::new(key, &args.model))
>     }
>     other => anyhow::bail!("unknown provider: {other}"),
> };
> ```
>
> ...剩下的接线和 `Arc<dyn Provider>` 一起工作。`AgentLoop::new` 的签名需要接受 `Arc<dyn Provider>` 或改成泛型。v0 用 `Arc<dyn Provider>` 更简单;之后想用零成本分发时再换成泛型。

下面是使用 `Arc<dyn Provider>` 形式的 **`main()` 剩余部分**:

```rust
    // ── 工具注册表 ────────────────────────────────────────
    let registry = Arc::new(
        InMemoryRegistry::new()
            .register(Arc::new(BashTool::new()))
    );

    // ── 循环 ─────────────────────────────────────────────────
    let loop_ = AgentLoop::new(
        provider,        // Arc<dyn Provider>
        registry,
        LoopConfig {
            max_iterations: args.max_iterations,
            system_prompt: Some(args.system),
            ..Default::default()
        },
    );

    // ── REPL ─────────────────────────────────────────────────
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    println!("hermes-cli v0.1 — type a message, Ctrl-D or Ctrl-C to quit");
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "/quit" || line == "/exit" { break; }

        let cancel = CancellationToken::new();
        // 接 Ctrl-C:实际应用用 `tokio::signal::ctrl_c()` 并 cancel 这个 token。
        // 第 6 阶段加上。

        let messages = vec![Message {
            role: Role::User,
            content: Content::Text(line.to_string()),
            reasoning: None,
            tool_call_id: None,
            tool_calls: None,
        }];

        let result = loop_.run(messages, cancel, |event| match event {
            LoopEvent::Thinking => {
                print!("… thinking");
                let _ = stdout.flush();
            }
            LoopEvent::AssistantMessage(_) => {
                println!();
            }
            LoopEvent::ToolCallStarted { call, .. } => {
                println!("\n→ tool: {}({})", call.name, call.arguments);
            }
            LoopEvent::ToolCallFinished { result, .. } => {
                match result {
                    Ok(out) => {
                        let preview: String = out.content.chars().take(200).collect();
                        println!("← {preview}{}", if out.content.len() > 200 { "…" } else { "" });
                    }
                    Err(e) => println!("← error: {e}"),
                }
            }
            LoopEvent::LengthLimit => println!("[hit length limit]"),
            LoopEvent::IterationsExhausted => println!("[max iterations]"),
            LoopEvent::Cancelled => println!("[cancelled]"),
        }).await;

        match result {
            Ok(r) => {
                if let Content::Text(s) = r.final_message.content {
                    println!("\n{s}");
                }
            }
            Err(e) => eprintln!("[loop error] {e}"),
        }
        println!();
    }

    Ok(())
}
```

---

## 8. Rust 特有的坑(按咬人的程度排序)

1. **trait 中的 `async`** — 必须用 `#[async_trait]`。原生 async-in-trait 只在 nightly 上。不要自己造轮子。

2. **在测试中 mock provider** — Python 写 `mock.patch("run_agent.OpenAI", ...)`。Rust 要么用 `mockall`,要么手写一个 `MockProvider: Provider` 结构体。**从设计 trait 开始就把这个做进去** — 别等到你有了 5 个 provider 才想起来。

3. **`Send + 'static`** — 任何 `tokio::spawn` 进去的东西必须满足这些 bounds。多数时候没问题。当出问题的时候,编译错误很冗长,但通常会精确指向需要 `Arc::clone` 的那个类型。

4. **错误类型** — 库 crate 用 `thiserror`,二进制用 `anyhow`。不要混用。从 `ProviderError` 到 `LoopError` 的 `?` 转换,正是第 2.2 节的 `From` 实现要消除的那种摩擦。

5. **Tracing** — 每个公开方法用 `#[instrument(skip(messages))]`,循环内部用 `tracing::info_span!`。Hermes 的 `hermes_logging.py` 自己造轮子;Rust 直接用 `tracing` 生态,不用重复造。

6. **`CancellationToken::clone()` 很便宜** — 它是 `Arc` 包装。到处传。不要自己造 `AtomicBool` "is cancelled" 标志。

7. **JSON Schema 验证** — `jsonschema` 是标准 crate,但编译一个 schema 大约 10µs;**在启动时对每个工具编译一次**,而不是每次调用。第 4 节的 `validate_args` 辅助函数对 v0 够用;之后在注册表里缓存编译好的 `JSONSchema`。

8. **OpenAI 把工具调用参数返回为 JSON 字符串,不是 JSON 对象。** 见 `openai.rs` 中的 `OaiFunctionCall.arguments: String`。你必须 `serde_json::from_str`。第一次遇到都会被坑。

9. **`serde(untagged)` enum** — `Content::Text(String)` vs `Content::Parts(Vec<ContentPart>)` 是 untagged 的,因为线上格式是 `string OR array`。tagged enum 在这里不工作。

---

## 9. 未来工作(第 5+ 阶段 — 仅草图)

### 9.1 流式(第 5 阶段)

覆盖 `Provider::stream` 返回 `CompletionStream`。第 4 节的循环需要一个 `run_streaming` 变体,逐个 yield `CompletionDelta`,并把它们组装成最终的 `Completion`。CLI:每收到一个 `content_delta` 就打印。Gateway:转发给用户,在 Telegram/Slack 中表现为"编辑消息"。

### 9.2 中断(第 6 阶段)

```rust
let cancel = CancellationToken::new();
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.ok();
    cancel.cancel();
});
```

把这个 `cancel` 传入 `loop_.run()`。`openai.rs` 和 `bash.rs` 中已经接好的 `select!` 会清理掉。

### 9.3 Skills(第 9 阶段)

`Skill` 是已知目录下的一种 `SKILL.md` 文件。加载器:

```rust
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,  // frontmatter 之后的 markdown
}

pub trait SkillLoader: Send + Sync {
    fn load_all(&self) -> Vec<Skill>;
}
```

`hermes-loop` 把 skill 描述注入到 system prompt 中,形式为
`"# Available skills\n\n- {name}: {description}\n..."`,并通过一个单独的 `SkillActivationTool` 让 LLM 按名字"加载"skill(body 被追加到消息里)。

Hermes 中的 `agent/skill_commands.py` 和 `tools/skills_hub.py` 是真正的参考;这个 Rust 版本是最低可用版本。

The current implementation is documented at `docs/superpowers/specs/2026-06-05-phase-9-skills-loading-design.md` and the implementation plan at `docs/superpowers/plans/2026-06-05-phase-9-skills-loading.md`.

### 9.4 Curator(第 12 阶段 — Hermes 的灵魂)

**curator 不是 agent 循环的一部分。** 它是一个独立的进程/后台任务,观察已完成的运行并决定:

1. 这个 run 是否应保存为一条记忆?
2. 是否应从这个 run 抽取出一个新 skill?
3. 是否应基于我们学到的东西改进已有的 skill?

用 Rust 表达就是:

```rust
#[async_trait]
pub trait Curator: Send + Sync {
    async fn observe(&self, run: &RunResult) -> Vec<Action>;
}

pub enum Action {
    SaveMemory { key: String, value: String },
    CreateSkill(Skill),
    UpdateSkill { name: String, new_body: String },
    NoOp,
}
```

curator 在 `tokio::spawn` 中对每个 `RunResult` 运行,并写入磁盘/数据库。它**不**阻塞 agent 循环。

---

## 10. 参考资料

- Hermes Python 源码 — `/Users/amagicpear/.hermes/hermes-agent/`
  - `run_agent.py` — AIAgent 类(我们要移植的就是这个)
  - `model_tools.py` + `toolsets.py` — 工具注册表模式
  - `agent/conversation_loop.py` — 抽离出来的对话循环
  - `AGENTS.md` — Hermes 自己的架构总结
- IronClaw(Rust agent OS,OpenClaw 的移植 — 部分参考)—
  https://github.com/nearai/ironclaw
  - `crates/ironclaw_engine/` — agent 循环对应物
  - `crates/ironclaw_llm/` — provider 适配器模式
  - `crates/ironclaw_dispatcher/` — 工具派发
  - `crates/ironclaw_skills/` — skill 加载
  - `crates/ironclaw_wasm/` — 沙箱(WASM)— 第 12+ 阶段的候选
- OpenAI Chat Completions API — https://platform.openai.com/docs/api-reference/chat
- `async-trait` crate — https://docs.rs/async-trait
- `jsonschema` crate — https://docs.rs/jsonschema
- `tracing` 生态 — https://docs.rs/tracing
