# Hermes Python vs Rust 移植 — phase 4 现状对照与 phase 5 前置审查

> 调研日期:2026-06-04
>
> 对照对象:`/Users/amagicpear/.hermes/hermes-agent/` (Hermes Python v0.15.1,约 20 万+ LOC)
>
> 对照基线:`/Users/amagicpear/projects/perry_hermes/` 当前 Rust 实现
>
> 当前阶段:phase 0-4 已基本实现(核心循环 + OpenAI 适配器 + BashTool + 运行时门面 + 交互式 CLI),下一步准备 phase 5 流式输出

## 1. 当前结论

Rust 版本已经不再是 phase 0 skeleton。当前实现已经具备一个最小可运行 agent 的核心路径:

1. `hermes-core`:消息、provider、tool、registry、usage、错误类型。
2. `hermes-loop`:完整工具调用循环,支持 `ToolUse` 后派发工具、追加 `role=tool` 消息、继续请求 LLM。
3. `hermes-providers`:Echo provider 和 OpenAI-compatible Chat Completions provider。
4. `hermes-tools`:真实 `BashTool`。
5. `hermes-runtime`:面向用户的 `AIAgent` facade,可组合 OpenAI-compatible provider + BashTool + AgentLoop。
6. 测试覆盖了 echo loop、OpenAI provider 基础解析、tool call 解析、tool call round-trip、参数校验失败后继续循环、bash 基础执行。

Phase 4 已经收口 **CLI 可用性、工具范围控制、进程稳定性、provider 边界行为** 中的多数项(详见下文“已解决”与“仍开放”两节)。下一步 phase 5 转入流式输出,以及未完成的权限语义。

## 2. 已实现内容对照

| 领域 | Rust 当前实现 | Hermes Python 对应 | 评估 |
|---|---|---|---|
| 消息模型 | `Message { role, content, reasoning, tool_call_id, tool_calls }` | OpenAI 风格 dict,message 上携带 reasoning/tool_calls | 对齐。reasoning 放在 message 上是正确方向 |
| Provider 抽象 | `Provider` trait + `EchoProvider` + `OpenAiProvider` | Python 中主要由 `AIAgent` 初始化时按 backend 分支选择 SDK/adapter | Rust 新设计,边界更清晰 |
| Tool 抽象 | `Tool` trait: `name` / `description` / `parameters_schema` / `toolset` / `execute` | `tools/registry.py` 注册 schema、handler、toolset、metadata | 已覆盖核心执行面,metadata 仍缺 |
| ToolRegistry | `InMemoryRegistry`,支持 `get` / `names` / `schemas` / `toolsets` / `tools_in_toolset` | Python module-level singleton registry | Rust 新设计,测试隔离更好 |
| Agent loop | `AgentLoop::run` 处理 Stop/Length/ContentFilter/Error/ToolUse | `run_agent.py` / `agent/conversation_loop.py` 的 ReAct 循环 | phase 3 核心路径已对齐 |
| 工具错误恢复 | 工具错误格式化成 `role=tool` 消息,循环继续 | `model_tools.handle_function_call()` 将异常转为工具结果文本 | 对齐,这是 agent 鲁棒性的关键 |
| OpenAI 工具调用 | 解析 `tool_calls`,下一轮 request round-trip assistant tool_calls | OpenAI Chat Completions 工具协议 | 已补上关键 round-trip |
| 参数校验 | dispatch 前用 JSON Schema 校验工具参数 | Python 主要依赖工具 handler/registry schema | Rust 实现更显式 |
| 取消 | `CancellationToken` 传入 provider/tool | Python `_interrupt_requested` 标志位 | Rust 新设计,方向正确 |
| runtime facade | `AIAgent::openai_compatible()` + `run_turn()` | `run_agent.py` 的用户入口 | phase 3 可用,但仍非常薄 |
| CLI | `hermes-cli` 已实现(交互式 REPL,clap 参数,多轮历史,事件渲染) | Hermes CLI/TUI 已很完整 | 最小可用形态已对齐,Phase 5+ 继续向 Hermes 完整度推进 |

## 3. 当前 crate 状态

### 3.1 `hermes-core`

已实现:

- `Message`, `Role`, `Content`, `ContentPart`, `ToolCall`
- `Provider`, `Completion`, `CompletionDelta`, `CompletionStream`, `FinishReason`
- `Tool`, `ToolContext`, `ToolPermissions`, `ToolOutput`, `Attachment`
- `ToolRegistry`, `InMemoryRegistry`, `ToolSchema`
- `ProviderError`, `ToolError`, `LoopError`
- `Usage`

需要更新设计认知:

- toolset 已经存在,不应再说“Toolset 概念缺失”。
- 真正缺的是 toolset 的 **session scope 生效路径**。
- `ToolPermissions` 已建模,但当前还没有强制执行语义。

### 3.2 `hermes-loop`

已实现:

- system prompt 注入。
- max iteration / wall-clock timeout / cancellation 检查。
- 每轮从 registry 获取 tool schema。
- provider complete 调用。
- assistant message 持久化。
- `FinishReason::ToolUse` 时执行工具并追加 tool result。
- 工具找不到、参数错误、执行错误都转为 `role=tool` 错误消息,不终止循环。
- `LoopEvent` 事件流。

与官方 Hermes 的差距:

- `LoopConfig.max_iterations: u32` 还不是 Hermes 那种 `IterationBudget` 对象,没有 refund / grace call / subagent budget。
- `parallel_tool_calls` 字段已公开但当前明确不生效。
- `dispatch_tool()` 没有按 enabled/disabled toolsets 限制可执行工具。
- `ToolContext` 里的 `session_id` / `working_dir` / `permissions` 仍由 loop 写死或默认,还没有由 runtime/CLI 注入。

### 3.3 `hermes-providers`

已实现:

- `EchoProvider` 用于离线 loop 测试。
- `OpenAiProvider` 支持 OpenAI-compatible `/chat/completions`。
- 支持 base_url 覆盖,可用于 MiniMax/Ollama/vLLM/proxy 等兼容端点。
- 解析 `stop` / `tool_calls` / `length` / `content_filter`。
- 解析 OpenAI 返回的 JSON 字符串形式 tool arguments。
- 序列化 assistant `tool_calls` 到下一轮请求体。
- 处理 401/429/非成功状态。

需要在 phase 4/5 前修:

- 未知 `finish_reason` 当前被当作 Stop,应改为 `InvalidResponse` 或 `FinishReason::Error`。
- `Content::Parts` 当前被静默丢成 `content: None`,应显式报错或正确映射多模态 content array。
- 空工具列表仍发送 `tool_choice: "auto"`,部分兼容端点可能拒绝。
- 429 暂时固定 retry-after=1,没有读取 header。
- 还没有 streaming。

### 3.4 `hermes-tools`

已实现:

- `BashTool`。
- JSON Schema 参数: `command`, `timeout_secs`。
- 支持 cancellation 和 timeout。
- 非零 exit code 以文本形式反馈给 LLM,不作为工具执行错误。
- 输出截断到上下文友好的大小。
- `toolset()` 返回 `"core"`。

必须修:

- 当前先 `child.wait().await`,再读取 stdout/stderr pipe。大输出可能填满 pipe 导致子进程阻塞,父进程等待退出,最终卡到 timeout。
- 输出截断使用 `&combined[..25_000]`,非 ASCII 边界可能 panic。
- 未检查 `ctx.permissions.subprocess`,权限模型没有真正生效。

### 3.5 `hermes-runtime`

已实现:

- `AIAgent::openai_compatible(api_key, model, base_url)`。
- 默认注册 `BashTool`。
- 默认 system prompt 提醒模型可用 bash。
- `run_turn(user_text, cancel, on_event)` 返回完整 `RunResult`。
- `examples/live_tool_use.rs` 可做 OpenAI-compatible + BashTool 的端到端 smoke。

phase 4 前需要补:

- runtime 构造参数需要表达工作目录、session id、权限策略、enabled/disabled toolsets。
- 目前 `AIAgent` 固定 provider 类型为 `OpenAiProvider`,后续 CLI 如果要支持 echo/openai 可先在 CLI 层分支,或让 runtime 提供 enum/factory。

### 3.6 `hermes-cli`

当前仍是 phase 0 stub:

- 只打印版本和 phase 0 提示。
- 没有接入 `hermes-runtime`。
- 没有 REPL。
- 没有 provider/model/base-url/api-key 参数。
- 没有工具列表/工具范围控制。

phase 4 的主要工作就在这里。

## 4. 官方 Hermes 设计中仍未覆盖的关键概念

这些不是 phase 4 全部要实现,但需要知道哪些会影响 CLI 设计。

### 4.1 Toolset session scope

官方 Hermes 中 toolset 不只是 metadata,而是 schema 暴露和 dispatcher 执行的共同过滤条件:

- `get_tool_definitions(enabled_toolsets, disabled_toolsets, quiet_mode)` 决定模型能看到哪些工具。
- `handle_function_call(..., enabled_toolsets, disabled_toolsets, ...)` 用同一组 toolsets 限制实际执行。
- 插件注册的工具也走同一条 toolset 解析路径。

Rust 当前已经有:

- `Tool::toolset()`
- `ToolRegistry::toolsets()`
- `ToolRegistry::tools_in_toolset()`

Rust 当前还缺:

- `ToolScope` 或 `LoopConfig.enabled_toolsets/disabled_toolsets`
- 按 scope 生成 schema 的接口
- dispatch 前验证 tool 是否在当前 scope 中
- CLI 参数: `--enabled-toolsets`, `--disabled-toolsets`, 或更简单的 `--tools safe/core/all`

### 4.2 Tool metadata

官方 registry entry 包含:

- `toolset`
- `schema`
- `handler`
- `check_fn`
- `requires_env`
- `emoji`
- async/sync 信息
- description

Rust 当前已有 toolset/schema/handler 等核心执行信息,但缺:

- `check()`
- `requires_env()`
- `emoji()`
- tool/toolset availability 查询

如果 phase 4 CLI 只做最小 REPL,metadata 可以先不做;如果要做 `hermes tools` 或友好错误提示,metadata 应在 phase 4 前半补上。

### 4.3 IterationBudget

官方 `agent/iteration_budget.py` 是独立对象,支持:

- consume
- refund
- used/remaining
- thread-safe
- subagent 独立预算
- budget 用完后的收尾调用语义

Rust 当前 `LoopConfig.max_iterations` 对 phase 3/4 足够,但 phase 7+ compression、phase 9 skills、phase 11 delegation/subagent 前应升级。

### 4.4 Session / persistence

官方 Hermes 有 SQLite/FTS5 session、历史搜索、checkpointing 等。Rust 当前只在 `RunResult.messages` 中返回轨迹,没有落盘。

phase 4 最小 CLI 可以不做持久化,但如果做多轮 REPL,至少需要在内存中维护 `Vec<Message>`。否则每次 `run_turn()` 都是单轮新会话。

## 5. phase 4 前必须处理的问题

按“会不会影响 CLI 可用性和安全语义”排序。

| 优先级 | 问题 | 影响 | 建议 |
|---|---|---|---|
| P0 | `BashTool` pipe 死锁风险 | CLI 用户执行大输出命令可能卡到 timeout | 改用 `wait_with_output()` 或并发读取 stdout/stderr |
| P0 | `ToolContext.permissions` 未强制执行 | CLI 暴露 shell 工具时权限语义不可信 | `BashTool` 检查 `subprocess`;loop/runtime 注入权限 |
| P0 | CLI 尚未接 runtime | phase 4 主目标 | `hermes run` 或默认 REPL 接 `AIAgent` |
| P1 | toolset scope 未接入 schema/dispatch | CLI 无法限制工具范围 | 引入 `ToolScope`;schema 和 dispatch 共用 |
| P1 | OpenAI unknown finish_reason 默认为 Stop | provider 异常可能伪装成正常答案 | 未知值返回 InvalidResponse |
| P1 | 空 tools 仍发送 tool_choice | 兼容端点可能失败 | 无工具时省略 `tools` 和 `tool_choice` |
| P1 | `Content::Parts` 静默丢弃 | 多模态输入会无声丢失 | 不支持时显式报错 |
| P2 | byte index 截断输出 | 中文/emoji 输出可能 panic | 使用 UTF-8 安全截断 |
| P2 | `parallel_tool_calls` 暴露但不生效 | API 误导 | phase 4 前删除或实现 |
| P2 | tool metadata 缺失 | `hermes tools`/友好提示不好做 | 增加 `check/requires_env/emoji` 默认方法 |

## 6. phase 4 CLI 建议范围

不要直接追 Hermes CLI/TUI 的完整复杂度。phase 4 最小可用范围建议是:

1. `hermes --provider echo` 本地 smoke。
2. `hermes --provider openai --model ... --base-url ...` 单轮运行。
3. 默认从 `OPENAI_API_KEY`, `OPENAI_MODEL`, `OPENAI_BASE_URL` 读取配置。
4. 支持 `--no-bash` 或 `--enabled-toolsets core/none` 的最小工具开关。
5. 支持 `--cwd` 设置工具工作目录。
6. Ctrl-C 触发 `CancellationToken`。
7. 输出 `LoopEvent`:thinking、tool start、tool finish、final answer。
8. 单元/集成测试覆盖 echo provider CLI path;OpenAI path 用 mock server 或 provider 层已有测试兜底。

暂不建议 phase 4 做:

- 持久化 session DB。
- 完整 TUI。
- skills。
- curator。
- 多平台 gateway。
- 插件系统。
- 完整 tool metadata UI。

## 7. 当前验证状态

已观察到的验证结果:

- `cargo clippy --all-targets --all-features -- -D warnings` 通过。
- `cargo test` 在普通环境通过。
- 沙箱内 `cargo test` 失败原因是测试绑定 `127.0.0.1:0` 启动 mock server 被限制,不是业务断言失败。

这说明 phase 3 的基础测试是健康的,但上述 P0/P1 问题仍是设计/实现缺口,不能因为测试通过而忽略。

## 8. 资源链接

- Hermes Python 源码:`/Users/amagicpear/.hermes/hermes-agent/`
- Rust 移植源码:`/Users/amagicpear/projects/perry_hermes/`
- Rust 设计文档:`plans/rust-port-design.md`

关键 Hermes Python 文件:

- `run_agent.py` — AIAgent 入口
- `agent/conversation_loop.py` — 抽离后的 conversation loop
- `model_tools.py` — tool definitions 与 function call dispatch
- `toolsets.py` — toolset 定义、alias、组合解析
- `tools/registry.py` — tool registry、metadata、availability
- `agent/iteration_budget.py` — iteration budget

关键 Rust 文件:

- `crates/hermes-core/src/message.rs`
- `crates/hermes-core/src/provider.rs`
- `crates/hermes-core/src/tool.rs`
- `crates/hermes-core/src/registry.rs`
- `crates/hermes-loop/src/agent.rs`
- `crates/hermes-providers/src/openai.rs`
- `crates/hermes-tools/src/bash.rs`
- `crates/hermes-runtime/src/lib.rs`
- `crates/hermes-cli/src/main.rs`
