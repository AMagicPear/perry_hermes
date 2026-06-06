# 调研:graniet/llm 作为 OpenAI Provider 复用的可行性

> 调研日期:2026-06-04
> 目标:评估能否用 [graniet/llm](https://github.com/graniet/llm) 替代我们设计文档第 7.13 节规划的自写 `OpenAiProvider`。

## 1. 仓库基本画像

| 项 | 值 |
|---|---|
| 当前版本 | 1.3.8(Cargo.toml) |
| License | MIT(宽松) |
| edition | 2021 |
| 维护活跃度 | 1.3.x 系列,持续在更 |
| 异步运行时 | tokio(`features = ["full"]`) |
| HTTP 客户端 | reqwest 0.12 |
| 默认 features | `["cli", "default-tls"]` — **不能用 default**,只启我们需要的 |

支持的 provider(共 16 个):OpenAI、Anthropic、Google(Gemini)、Ollama、DeepSeek、xAI、Phind、Groq、Azure OpenAI、OpenRouter、Cohere、Mistral、HuggingFace、Bedrock、ElevenLabs、Phind。每个 backend 都是 feature-gated,只开 `openai` 编译开销可控。

## 2. 关键 API 形状

### 2.1 Provider 层

```rust
// src/lib.rs
pub trait LLMProvider:
    chat::ChatProvider
    + completion::CompletionProvider
    + embedding::EmbeddingProvider
    + stt::SpeechToTextProvider
    + tts::TextToSpeechProvider
    + models::ModelsProvider
{ }
```

通过 `LLMBuilder` 配置(温度、max_tokens、tools、timeout、web search、retry 等),`build()` 返回 `Box<dyn LLMProvider>`。

### 2.2 Chat 接口

```rust
// src/chat/traits.rs
#[async_trait]
pub trait ChatProvider: Sync + Send {
    async fn chat(&self, messages: &[ChatMessage]) -> Result<Box<dyn ChatResponse>, LLMError>;

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[Tool]>,
    ) -> Result<Box<dyn ChatResponse>, LLMError>;

    async fn chat_stream(&self, messages: &[ChatMessage])
        -> Result<Pin<Box<dyn Stream<Item=Result<String,LLMError>> + Send>>, LLMError>;

    async fn chat_stream_with_tools(&self, messages: &[ChatMessage], tools: Option<&[Tool]>)
        -> Result<Pin<Box<dyn Stream<Item=Result<StreamChunk,LLMError>> + Send>>, LLMError>;
    // ... 另带 chat_with_web_search / chat_stream_struct
}
```

### 2.3 消息类型(注意 role 限制)

```rust
// src/chat/message.rs
pub enum ChatRole { User, Assistant }   // ⚠️ 只有两种 role!
pub enum MessageType {
    Text, Image(..), Pdf(..), Audio(..), ImageURL(..),
    ToolUse(Vec<ToolCall>), ToolResult(Vec<ToolCall>),
}
```

**关键发现:`ChatRole` 只有 `User` 和 `Assistant`,没有 `System` 和 `Tool`。**
- System prompt 通过 `LLMBuilder.system()` 配置,不进 message 流
- 工具结果用 `ChatMessage::user().tool_result(...)` 表示 — 把 tool result 伪装成 user message
- 助手侧的 `tool_use` 用 `MessageType::ToolUse(Vec<ToolCall>)` 挂在 assistant message 上

### 2.4 工具类型

```rust
// src/chat/tool.rs
pub struct Tool {
    pub tool_type: String,        // "function"
    pub function: FunctionTool,
    pub cache_control: Option<Value>,  // Anthropic 提示缓存
}

pub struct FunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,         // 直接是 JSON Schema Value
}

pub enum ToolChoice { Any, Auto, Tool(String), None }
```

工具定义格式与 OpenAI Chat Completions 一致(我们设计文档里的 `OaiTool`/`OaiFunctionDef` 几乎一样)。

### 2.5 Usage

```rust
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub completion_tokens_details: Option<CompletionTokensDetails>,  // 含 reasoning_tokens
    pub prompt_tokens_details: Option<PromptTokensDetails>,          // 含 cached_tokens
}
```

`prompt_tokens_details.cached_tokens` 正是我们 `Usage.cached_input_tokens` 需要的数据。

### 2.6 OpenAI 后端实现路径

```rust
// src/backends/openai.rs
pub struct OpenAI {
    provider: OpenAICompatibleProvider<OpenAIConfig>,  // 委托给 generic provider
    pub enable_web_search: bool,
    pub web_search_* : Option<String>,
}

impl OpenAIProviderConfig for OpenAIConfig {
    const PROVIDER_NAME: &'static str = "OpenAI";
    const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1/";
    const DEFAULT_MODEL: &'static str = "gpt-4.1-nano";
    const SUPPORTS_REASONING_EFFORT: bool = true;
    const SUPPORTS_PARALLEL_TOOL_CALLS: bool = false;
    const SUPPORTS_STREAM_OPTIONS: bool = true;
}
```

**⚠️ 端点分歧**:graniet/llm 的 OpenAI 后端使用 OpenAI 较新的 **`/responses` API**,而我们设计文档第 7.13 节写的是 **`/chat/completions`**。两者数据模型不同(Responses API 有 `input`/`instructions` 字段,没有 `messages` 数组),迁移到 Responses API 是一次性 schema 差异。

## 3. 与我们 `Provider` trait 的契合度矩阵

| 我们 trait 的能力 | graniet/llm 的对应 | 评估 |
|---|---|---|
| `complete(messages, tools, cancel)` | `chat_with_tools(messages, tools)` | ✅ 接口同构,**但缺 `cancel` 参数** |
| `complete()` 返回 `Completion { message, usage, finish_reason }` | 返回 `Box<dyn ChatResponse>`,通过 `.text()` / `.tool_calls()` / `.usage()` 访问 | ⚠️ **没有 `finish_reason` 字段**,得自己从响应里推断 |
| `ToolCall { id, name, arguments: Value }` | `ToolCall { id, call_type, function: FunctionCall { name, arguments: String } }` | ⚠️ `arguments` 是 `String` 不是 `Value`,要自己 `from_str` |
| 流式 `stream()` → `CompletionStream` | `chat_stream_with_tools()` → `Stream<StreamChunk>`(区分 Text / ToolUseStart / ToolUseInputDelta / ToolUseComplete / Done) | ✅ **比设计文档更精细**,phase 5 的活省了 |
| `CancellationToken` | 无 | ❌ **致命缺失** |
| `FinishReason` (Stop/ToolUse/Length/ContentFilter/Error) | 隐式(没有显式枚举,ToolUse 通过 `tool_calls()` 是否非空判断) | ⚠️ 没有 `Length`/`ContentFilter` 区分 |
| `Usage { input, output, cached_input }` | `Usage { prompt, completion, total, ..._details }` | ✅ 完全够用 |
| 错误 `ProviderError { RateLimited{retry_after_secs}, ContextLengthExceeded, Auth, Transport, ... }` | `LLMError { HttpError, AuthError, InvalidRequest, ProviderError, ResponseFormatError, JsonError, ToolConfigError, RetryExceeded }` | ⚠️ 没有 `RateLimited`/`ContextLengthExceeded` 显式变体,要靠 HttpError 字符串解析或保留细节 |
| `tools: &[ToolSchema]`(我们用 JSON Schema) | `tools: Option<&[Tool]>`(graniet/llm 自己的 `Tool` 类型) | ✅ 几乎 1:1,只是换名字 |
| 多 provider(Anthropic、Bedrock…) | 同 crate 内置 16 个 | 🎁 **巨大加分项** — phase 8 的 Anthropic、phase 11 后端的多模型选择都白拿 |

## 4. 直接结论

### 能复用吗?
**能,但要写适配器(adapter),不能 1:1 直接用。**

graniet/llm 的 `ChatProvider` 和我们的 `Provider` trait 在概念上对齐,但 API 表面、错误类型、取消语义、role 模型都不同。它是一个**对外的高级 SDK**,不是一个**可被实现的 trait** — 我们没法 `impl Provider for graniet::OpenAI`。

### 节省的工程量
如果走"在 graniet/llm 上加一层适配器"的路线,我们能省下:

- **OpenAI HTTP 请求构造**(~150 行)— 他们已经做了
- **OpenAI 响应反序列化**(~100 行)— 他们已经做了
- **`finish_reason` 字符串映射**(~10 行)— 他们隐式做了
- **工具调用的 JSON 字符串 → `serde_json::Value`**(~20 行)— 我们要的
- **流式 SSE 解析**(~150 行)— 他们的 `chat_stream_with_tools` 写好了,而且比设计文档里的方案更好(粒度到 `StreamChunk` 枚举)
- **重试/退避**— `resilient_llm` 模块免费提供
- **Phase 8 (Anthropic)、第 8 阶段后的所有多 provider** — 几乎白拿

合计约 **500–800 LOC 不需要写**。

### 要付出的工程量
适配层大约 **150–250 LOC**:

1. **类型映射** — `Message`(我们的) ↔ `ChatMessage`(他们的),`ToolSchema` ↔ `Tool`,`Usage` ↔ `Usage`
2. **role 映射** — 把 `Role::System` 合并进 builder,把 `Role::Tool` 用 `MessageType::ToolResult` 表示
3. **错误映射** — `LLMError` → `ProviderError`,用 `From` impl
4. **取消适配** — 这是最大坑(见下)
5. **`FinishReason` 推断** — 我们的 loop 依赖 `Stop`/`ToolUse`/`Length`/`ContentFilter`/`Error` 五个变体,graniet/llm 没暴露这个枚举,得从响应里看 `tool_calls` 是否非空来推断 `ToolUse`,其余全部当 `Stop`

### 关键阻碍:**取消语义**
我们的设计文档第 8 节第 1 条强调:

> **`async` in trait** — 必须用 `#[async_trait]`。原生 async-in-trait 只在 nightly 上。
>
> 必须在 `complete()` 末尾 `tokio::select!` 监听 `cancel.cancelled()`,让 Ctrl-C 能立即中断 LLM 调用。

graniet/llm 的 `chat_with_tools()` **完全不接 cancellation**。一旦发出去的 HTTP 请求被丢(比如我们 `tokio::select!` 时另一支赢),底层的 `reqwest` 调用**可能还在后台跑**(取决于 future 被 drop 的位置和 reqwest 内部 cancel 信号传递)。在长上下文/慢响应场景下,这意味着用户按 Ctrl-C 后,网络连接可能要几秒甚至几十秒才彻底释放。

**绕开办法**(都不完美):
- **A**:我们包一层 `tokio::select!`,在 cancel 触发时**立即返回** `ProviderError::Cancelled` 给循环,但底层 future 不一定被立刻取消 — 用户感知到的延迟降了,但底层连接仍在
- **B**:在 adapter 里**给 reqwest 客户端设置短 timeout**(比如 5s),cancel 触发后让请求自然超时
- **C**:放弃,自己在 hermes-providers 里重写 OpenAI 后端,把省下来的工程量用别的方式补回来

### 关键阻碍:`finish_reason` 缺失
我们的循环依赖 `match finish_reason { Stop | Length | ContentFilter | ToolUse | Error }`,graniet/llm 把它藏在响应里:

- `ToolUse` → `response.tool_calls()` 非空
- `Stop` → `response.tool_calls()` 为空 且有文本
- `Length` / `ContentFilter` / `Error` → **没有显式信号**!

如果用户用 graniet/llm,我们的 `FinishReason::Length` 分支永远走不到 — Hermes Python 的"回答被截断时给个警告"行为会丢。要么接受这个妥协(把 Length 当 Stop 处理),要么 adapter 自己解析 OpenAI 响应(但这样就退化成了"半自己写")。

## 5. 三种集成方案对比

### 方案 A:全适配(推荐用于 phase 0–4)
```
hermes-loop → hermes-core::Provider (我们的 trait)
                ↑
                | adapter (~200 LOC)
                |
            graniet_llm::Box<dyn LLMProvider>
```
- 保住我们的 `Provider` trait 不动 — 未来要换实现不用改 loop
- Phase 0–4 跑通,工作量最少
- 代价:第 5 阶段(流式)用 graniet/llm 的 `StreamChunk` 而不是我们自己设计的 `CompletionDelta`;第 6 阶段(中断)的体验会打折扣

### 方案 B:混合(最稳)
- Phase 0–4 用 graniet/llm(适配器)
- Phase 5+ 流式/中断时,**重新评估** — 如果 graniet/llm 的限制开始咬人,给 `hermes-providers/src/openai.rs` 写一个原生 ~600 LOC 实现替换掉 adapter(把设计文档第 7.13 节的代码搬过来)
- 缺点:第 5 阶段会有一次大重构

### 方案 C:不用(最干净)
- 完全按设计文档第 7.13 节写 `hermes-providers/src/openai.rs`
- 优点:控制力最强,符合设计文档原意
- 缺点:写 600 LOC,对"几个周末做出 fun agent"的目标是负向贡献

## 6. 我的建议

**采用方案 B**:把 graniet/llm 作为 phase 0–4 的快速通道,phase 5+ 视需要替换。

理由:
1. 设计文档的目标是"几个周末做出可用的 agent",graniet/llm 直接给我们一个能用的 OpenAI 客户端 + 14 个其他 provider,符合目标
2. 我们仍然保住了 `Provider` trait 的抽象边界 — adapter 是 hermes-providers 的内部细节
3. 取消语义的损失在 phase 0–4 几乎无影响(早期是 REPL,Ctrl-C 急迫性低)
4. 等到 phase 5+ 真要流式 + 强取消时,有完整的设计文档可以参考着写原生实现

**具体动作**:
- `crates/hermes-providers/Cargo.toml` 加 `llm = { version = "1.3", default-features = false, features = ["openai"] }`
- 在 `crates/hermes-providers/src/graniet_adapter.rs` 写适配器,把 `Box<dyn llm::LLMProvider>` 包成我们的 `OpenAiProvider`
- 设计文档第 7.13 节的 `OpenAiProvider` 改名 `NativeOpenAiProvider`,标记为"phase 5+ 替换方案"
- 在文档里加一条 note,说明 Hermes 的设计原则不受影响(我们仍然持有 Provider trait 的完全控制权)

## 7. 风险与未确认事项

- ⚠️ `graniet/llm` OpenAI 后端用 `/responses` API,不是 `/chat/completions`。两者数据模型不同,**OAI reasoning_effort、structured output、function calling 行为细节有差**。如果未来要支持 `/chat/completions` 协议的本地模型(Ollama、vLLM),graniet/llm 的 OpenAI adapter 不能直接复用 — 但它的 `OpenAICompatibleProvider` 抽象或许能用
- ⚠️ `ChatMessage` 没有 `System` / `Tool` role。system prompt 走 builder,tool result 走 `MessageType::ToolResult` 挂在 user message 上。我们 adapter 要小心处理边界(尤其 loop 里把 `Role::Tool` 消息回填时)
- ⚠️ v1.3.8 是单一 maintainer 项目(Tristan Granier),bus factor 低。我们 fork + 自维护的能力存在,但优先用 upstream
- ⚠️ 编译时间:即使只开 `openai` feature,graniet/llm 的依赖树(reqwest、tokio、secrecy、serde_yaml、toml、dirs…)会把 hermes-providers 的首次编译时间拉长几秒。后续增量编译可接受

## 8. 资源链接

- 仓库:https://github.com/graniet/llm
- docs.rs:https://docs.rs/llm
- 关键文件:
  - `src/lib.rs` — `LLMProvider` trait 总入口
  - `src/chat/traits.rs` — `ChatProvider` / `ChatResponse` trait
  - `src/chat/message.rs` — `ChatMessage` / `ChatRole`(注意只有 User/Assistant)
  - `src/chat/tool.rs` — `Tool` / `ToolChoice`
  - `src/chat/stream.rs` — `StreamChunk` 枚举
  - `src/chat/usage.rs` — `Usage`
  - `src/error.rs` — `LLMError`
  - `src/backends/openai.rs` — `OpenAI` 后端(委托给 `OpenAICompatibleProvider`)
  - `examples/unified_tool_calling_example.rs` — 多 provider 工具调用完整示例
  - `examples/openai_streaming_example.rs` — OpenAI 流式示例
