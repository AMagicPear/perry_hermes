# Hermes Rust

> Vibe code 一个 Rust 版的 Nous Research 的 [hermes-agent](https://github.com/NousResearch/hermes-agent) —— 一个自进化的 AI Agent。

当前进度：**Phase 0–3 已完成**（核心循环 + OpenAI 适配器 + BashTool + 运行时门面）。CLI 交互界面将在 Phase 4 实现。

## 特性

- **ReAct 风格 Agent 循环** — LLM 决策 → 工具调用 → 结果反馈 → 继续决策，直到任务完成
- **工具错误非致命** — 执行失败不会崩溃循环，而是把错误信息反馈给 LLM，让它自行调整策略
- **OpenAI 兼容** — 通过 `with_base_url()` 支持 OpenAI、DeepSeek、MiniMax、Ollama、vLLM 等任意兼容端点
- **协作式取消** — `CancellationToken` 贯穿所有异步调用，支持 Ctrl-C 优雅中断
- **严格的分层架构** — 依赖方向始终向下，无循环依赖

## 架构

```
hermes-cli (二进制入口, Phase 4)
  └─ hermes-runtime (产品 API 门面 — AIAgent)
       └─ hermes-loop (Agent 循环状态机)
            ├─ hermes-core (类型、特征、错误 — 无 IO)
            ├─ hermes-providers (OpenAI 适配器、Echo 模拟)
            └─ hermes-tools (BashTool)
```

### 核心 Crate

| Crate | 职责 | 关键类型/特征 |
|---|---|---|
| `hermes-core` | 核心类型、特征定义、错误类型，无 IO | `Provider`, `Tool`, `ToolRegistry`, `Message`, `Completion` |
| `hermes-providers` | LLM 提供者实现 | `OpenAiProvider`, `EchoProvider` |
| `hermes-tools` | 内置工具实现 | `BashTool`（bash 命令执行，超时 30s，输出截断 50KB） |
| `hermes-loop` | Agent 循环状态机 | `AgentLoop<P, R>`, `LoopConfig`, `RunResult` |
| `hermes-runtime` | 用户面向的 API 门面 | `AIAgent::openai_compatible()`, `run_turn()` |

### 三个核心特征

- **`Provider`** — 异步 `complete(messages, tools, cancel) -> Completion`，LLM 调用的统一抽象
- **`Tool`** — 异步 `execute(args, ctx, cancel) -> ToolOutput`，工具调用的统一抽象
- **`ToolRegistry`** — 工具名到 `Arc<dyn Tool>` 的映射，支持按 toolset 过滤

## 快速开始

### 环境要求

- Rust 1.75+（MSRV）
- `direnv`（可选，自动加载环境变量）

### 配置环境变量

复制 `.envrc.example` 为 `.envrc`，填入：

```bash
export OPENAI_API_KEY="sk-..."
export OPENAI_BASE_URL="https://api.openai.com/v1"  # 可选，默认 OpenAI
export OPENAI_MODEL="gpt-4o-mini"                     # 可选，默认 gpt-4o-mini
```

支持 OpenAI 兼容端点，例如 DeepSeek、MiniMax、Ollama 等，只需修改 `OPENAI_BASE_URL`。

### 构建

```bash
cargo build
```

### 运行示例

```bash
# 纯 LLM 调用（无工具）
cargo run -p hermes-providers --example live_smoke -- "say hi"

# 带工具调用的完整 Agent
cargo run -p hermes-runtime --example live_tool_use -- "what time is it?"
```

## 开发

### 构建与测试

```bash
cargo build                              # 构建所有 crate
cargo test                               # 运行所有测试
cargo test -p hermes-core                # 测试单个 crate
cargo test -p hermes-loop --test tool_dispatch  # 运行单个集成测试
cargo clippy --all-targets --all-features -- -D warnings  # Lint
```

### 测试结构

```
crates/hermes-loop/tests/
  ├── echo_loop.rs          # Echo 模拟循环测试
  ├── tool_dispatch.rs      # 工具调度集成测试
  └── arg_validation.rs     # 工具参数校验测试

crates/hermes-providers/tests/
  ├── openai.rs             # OpenAI 适配器 HTTP 测试（httpmock）
  └── tool_call_roundtrip.rs  # tool_calls 序列化往返测试

crates/hermes-tools/tests/
  └── bash.rs               # BashTool 实际执行测试
```

项目采用 **TDD 工作流**：RED → GREEN → REFACTOR，严格先写失败测试再写实现代码。

## 设计文档

| 文档 | 内容 |
|---|---|
| [plans/rust-port-design.md](plans/rust-port-design.md) | 主设计文档：类型定义、特征设计、循环实现、12 阶段路线图 |
| [plans/hermes-comparison.md](plans/hermes-comparison.md) | Rust 移植与 Python 原版的差异对比 |
| [CLAUDE.md](CLAUDE.md) | Claude Code 的开发指引 |

## 路线图

| 阶段 | 目标 | 状态 |
|---|---|---|
| Phase 0 | 骨架：可编译的空工作空间 + 特征定义 | ✅ |
| Phase 1 | Echo 循环：Mock Provider 返回 Stop，循环跑一次 | ✅ |
| Phase 2 | OpenAI Provider：真实 gpt-4o-mini 调用 | ✅ |
| Phase 3 | BashTool：真实 shell 命令执行 | ✅ |
| Phase 4 | CLI：交互式 REPL | 🔜 |
| Phase 5 | 流式输出：逐 token 输出 | |
| Phase 6 | 中断：Ctrl-C 停止流式输出 | |
| Phase 7 | 上下文压缩：消息过长时自动摘要 | |
| Phase 8 | Anthropic Provider：多 Provider 支持 | |
| Phase 9 | Skills：加载 .md 文件作为系统提示词 | |
| Phase 10 | TUI：ratatui 多行编辑器 | |
| Phase 11 | 平台网关：Telegram（grammY-rs） | |
| Phase 12 | Curator：学习循环（Hermes 的灵魂） | |

## 已知问题

- BashTool 大量 stdout 输出可能阻塞管道
- `ToolContext.permissions` 未强制执行
- CLI 未接入运行时
- Toolset 过滤未接入 schema/调度
- 未知 `finish_reason` 默认为 Stop
- `Content::Parts` 被静默丢弃
- 空工具列表仍发送 `tool_choice: "auto"`

## License

MIT
