# Hermes Rust

> Vibe code 一个 Rust 版的 Nous Research 的 [hermes-agent](https://github.com/NousResearch/hermes-agent) —— 一个自进化的 AI Agent。

当前进度：**Phase 0–6 已完成**（核心循环 + OpenAI 适配器 + BashTool + 运行时门面 + 交互式 CLI + 流式输出 + Ctrl-C 中断）。下一步进入 Phase 7：上下文压缩 / 预算模型。

## 特性

- **ReAct 风格 Agent 循环** — LLM 决策 → 工具调用 → 结果反馈 → 继续决策，直到任务完成
- **工具错误非致命** — 执行失败不会崩溃循环，而是把错误信息反馈给 LLM，让它自行调整策略
- **OpenAI 兼容** — 通过 `with_base_url()` 支持 OpenAI、DeepSeek、MiniMax、Ollama、vLLM 等任意兼容端点
- **协作式取消** — `CancellationToken` 贯穿所有异步调用，支持 Ctrl-C 优雅中断(第一次中断当前 turn，第二次退出 REPL)
- **交互式 REPL CLI** — 多轮对话、工具调用实时渲染(emoji + 截断预览)、`/quit`/`/exit` 斜杠命令、`--disabled-toolsets` 细粒度控制
- **Toolset 过滤** — 通过 runtime 配置按 toolset 名启用/禁用工具(目前内置 `core` / `terminal`)
- **健壮的 BashTool** — stdout/stderr 并发 drain 避免管道死锁；输出采用 head+tail 40%/60% 截断策略，与 Python Hermes 对齐
- **严格的分层架构** — 依赖方向始终向下，无循环依赖

## 架构

```
hermes-cli (交互式 REPL — Phase 4)
  └─ hermes-runtime (产品 API 门面 — AIAgent)
       └─ hermes-loop (Agent 循环状态机)
            ├─ hermes-core (类型、特征、错误 — 无 IO)
            ├─ hermes-providers (OpenAI 适配器、Echo 模拟)
            └─ hermes-tools (BashTool)
```

### 核心 Crate

| Crate | 职责 | 关键类型/特征 |
|---|---|---|
| `hermes-core` | 核心类型、特征定义、错误类型，无 IO | `Provider`, `Tool`, `InMemoryRegistry`, `Message`, `Completion`, `FinishReason` |
| `hermes-providers` | LLM 提供者实现 | `OpenAiProvider` (兼容 DeepSeek/MiniMax/Ollama/vLLM), `EchoProvider` |
| `hermes-tools` | 内置工具实现 | `BashTool` (bash 命令执行，30s 超时，50KB 输出截断，并发 drain stdout/stderr) |
| `hermes-loop` | Agent 循环状态机 | `AgentLoop`, `LoopConfig`, `RunResult`, `LoopEvent` |
| `hermes-runtime` | CLI/gateway 共用运行入口 | `AIAgent`, `AgentOptions`, `run_messages()`, `run_turn()` |
| `hermes-cli` | 交互式 REPL 二进制 | `hermes` 命令，clap 参数解析，多轮历史，事件渲染 |

### 核心边界

- **`Provider`** — 异步 `stream(messages, tools, cancel) -> CompletionStream`，LLM 调用的统一抽象
- **`Tool`** — 异步 `execute(args, ctx, cancel) -> ToolOutput`，工具调用的统一抽象
- **`InMemoryRegistry`** — 工具名到 `Arc<dyn Tool>` 的简单映射，运行时负责按 toolset 过滤
- **`LoopEvent`** — agent 到 CLI/gateway 的展示事件；平台适配器决定怎么渲染

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

### 运行 CLI

```bash
# 真实 LLM + bash 工具
cargo run -p hermes-cli

# 离线烟囱测试：用 echo provider 模拟 LLM，不发任何 HTTP 请求
cargo run -p hermes-cli -- --provider echo

# 切换模型/端点(也支持 DeepSeek / MiniMax / Ollama / vLLM 等任意 OpenAI 兼容服务)
cargo run -p hermes-cli -- --base-url https://api.deepseek.com/v1 --model deepseek-chat

# 禁用 bash(关闭 terminal toolset,只做对话)
cargo run -p hermes-cli -- --disabled-toolsets terminal

# 在指定工作目录下执行 shell 命令
cargo run -p hermes-cli -- --cwd /tmp

# 调高最大迭代次数(默认 20)
cargo run -p hermes-cli -- --max-iterations 50
```

REPL 内可用命令：

- 输入任意文本 → 提交给 agent
- `/quit` / `/exit` → 退出
- `Ctrl-C` 第一次 → 取消当前 turn；第二次 → 退出 REPL
- `Ctrl-D` → 退出

### 运行示例

```bash
# 纯 LLM 调用（无工具）
cargo run -p hermes-providers --example live_smoke -- "say hi"

# 带工具调用的完整 Agent(单 turn,程序化)
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

### CLI 自检

```bash
# 离线烟囱测试：确认 REPL 能正常启动、echo provider 能跑通整个循环
echo "hello" | cargo run -p hermes-cli --quiet -- --provider echo
```

### 测试结构

```
crates/hermes-loop/tests/
  ├── echo_loop.rs          # Echo 模拟循环测试
  ├── tool_dispatch.rs      # 工具调度集成测试
  ├── arg_validation.rs     # 工具参数校验测试
  ├── usage_metrics.rs      # streaming usage 指标测试
  └── support/              # loop 集成测试共享 provider

crates/hermes-providers/tests/
  ├── openai.rs             # OpenAI 适配器 HTTP 测试（httpmock）
  ├── openai_stream.rs      # SSE streaming 测试
  └── tool_call_roundtrip.rs  # tool_calls 序列化往返测试

crates/hermes-tools/tests/
  └── bash.rs               # BashTool 实际执行测试
```

项目采用 **TDD 工作流**：RED → GREEN → REFACTOR，严格先写失败测试再写实现代码。

## 设计文档

| 文档 | 内容 |
|---|---|
| [plans/rust-port-design.md](plans/rust-port-design.md) | 历史主设计草稿：早期类型定义、特征设计、12 阶段路线图；部分 API 已被当前实现简化 |
| [plans/hermes-comparison.md](plans/hermes-comparison.md) | Rust 移植与 Python 原版的差异对比；顶部有当前状态修订 |
| [CLAUDE.md](CLAUDE.md) | Claude Code 的开发指引 |

## 路线图

| 阶段 | 目标 | 状态 |
|---|---|---|
| Phase 0 | 骨架：可编译的空工作空间 + 特征定义 | ✅ |
| Phase 1 | Echo 循环：Mock Provider 返回 Stop，循环跑一次 | ✅ |
| Phase 2 | OpenAI Provider：真实 gpt-4o-mini 调用 | ✅ |
| Phase 3 | BashTool：真实 shell 命令执行 | ✅ |
| Phase 4 | CLI：交互式 REPL，clap 参数，多轮历史，事件渲染 | ✅ |
| Phase 5 | 流式输出：逐 token 输出 | ✅ |
| Phase 6 | 中断：Ctrl-C 停止流式输出并保留 partial assistant message | ✅ |
| Phase 7 | 上下文压缩：消息过长时自动摘要 | |
| Phase 8 | Anthropic Provider：多 Provider 支持 | |
| Phase 9 | Skills：加载 .md 文件作为系统提示词 | |
| Phase 10 | TUI：ratatui 多行编辑器 | |
| Phase 11 | 平台网关：Telegram（grammY-rs） | |
| Phase 12 | Curator：学习循环（Hermes 的灵魂） | |

## 已知问题

- `ToolContext.permissions` 已建模但未强制执行
- 未知 `finish_reason` 会映射为 `FinishReason::Error`，但错误信息仍较粗
- OpenAI-compatible provider 已支持 `Content::Parts` 的 text/image_url content array；其他多媒体 part 类型尚未建模
- 复用同一 `BashTool` 时 `child.kill().await` 的并发安全未充分测试

## License

MIT
