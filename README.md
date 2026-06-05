# Hermes Rust

> Vibe code 一个 Rust 版的 Nous Research 的 [hermes-agent](https://github.com/NousResearch/hermes-agent) —— 一个自进化的 AI Agent。

当前进度：**Phase 0–6、Phase 8–9 已完成**（核心循环 + OpenAI/Anthropic 适配器 + BashTool + 运行时门面 + 交互式 CLI + 流式输出 + Ctrl-C 中断 + TOML provider/agent 配置 + Skills 加载）。Phase 7 上下文压缩仍暂缓。

## 特性

- **ReAct 风格 Agent 循环** — LLM 决策 → 工具调用 → 结果反馈 → 继续决策，直到任务完成
- **工具错误非致命** — 执行失败不会崩溃循环，而是把错误信息反馈给 LLM，让它自行调整策略
- **OpenAI 兼容** — 通过 `with_base_url()` 支持 OpenAI、DeepSeek、MiniMax、Ollama、vLLM 等任意兼容端点；普通 content 中的 `<think>...</think>` fallback 会被归一化为 reasoning
- **Anthropic 兼容** — 支持 Anthropic Messages API，以及 MiMo 这类需要 `api-key` header 的兼容端点
- **TOML 配置文件** — CLI 启动时必须有一个 `HermesConfig`(TOML)。查找顺序:`--config` 显式路径 → `~/.perry_hermes/config.toml` → `./hermes.toml`;三者皆无则报错退出
- **协作式取消** — `CancellationToken` 贯穿所有异步调用，支持 Ctrl-C 优雅中断(第一次中断当前 turn，第二次退出 REPL)
- **交互式 REPL CLI** — 多轮对话、工具调用实时渲染(emoji + 截断预览)、`/quit`/`/exit` 斜杠命令、`[agent].disabled_toolsets` 细粒度控制
- **Toolset 过滤** — 通过 runtime 配置按 toolset 名启用/禁用工具(目前内置 `core` / `terminal`)
- **Skills 加载** — `~/.perry_hermes/skills/` 下的 `.md` skill 文件在运行时自动加载，名称和描述注入 system prompt
- **健壮的 BashTool** — stdout/stderr 并发 drain 避免管道死锁；输出采用 head+tail 40%/60% 截断策略，与 Python Hermes 对齐
- **严格的分层架构** — `hermes-core` 保持传输无关；`hermes-runtime` 只保留薄组装入口，内部职责集中在 `agent`、`prompting`、`provider_factory`、`tool_catalog` 几个模块

## 架构

```
hermes-cli (交互式 REPL — Phase 4)
  └─ hermes-runtime (产品 API 门面 — AIAgent)
       └─ hermes-loop (Agent 循环状态机)
            ├─ hermes-core (类型、特征、错误 — 无 IO)
            ├─ hermes-providers (OpenAI / Anthropic 适配器、Echo 模拟)
            ├─ hermes-tools (BashTool)
            └─ hermes-skills (SKILL.md 加载 + system prompt 注入)
```

### 核心 Crate

| Crate | 职责 | 关键类型/特征 |
|---|---|---|
| `hermes-core` | 核心类型、特征定义、错误类型，无 IO | `Provider`, `Tool`, `InMemoryRegistry`, `Message`, `Completion`, `FinishReason` |
| `hermes-providers` | LLM 提供者实现 | `OpenAiProvider` (兼容 DeepSeek/MiniMax/Ollama/vLLM), `AnthropicProvider`, `EchoProvider` |
| `hermes-tools` | 内置工具实现 | `BashTool` (bash 命令执行，30s 超时，50KB 输出截断，并发 drain stdout/stderr) |
| `hermes-loop` | Agent 循环状态机 | `AgentLoop`, `LoopConfig`, `RunResult`, `LoopEvent` |
| `hermes-runtime` | CLI/gateway 共用运行入口，内部按组装职责拆分 | `AIAgent`, `HermesConfig`, `SessionContext`, `run_messages`, `run_turn` |
| `hermes-cli` | 交互式 REPL 二进制 | `hermes` 命令，clap 参数解析，多轮历史，事件渲染 |

### 核心边界

- **`Provider`** — 异步 `stream(messages, tools, cancel) -> CompletionStream`，LLM 调用的统一抽象
- **`Tool`** — 异步 `execute(args, ctx, cancel) -> ToolOutput`，工具调用的统一抽象
- **`InMemoryRegistry`** — 工具名到 `Arc<dyn Tool>` 的简单映射，运行时负责按 toolset 过滤
- **`LoopEvent`** — agent 到 CLI/gateway 的展示事件；平台适配器决定怎么渲染
- **`ProviderError::Transport`** — 在 `hermes-core` 中保持传输无关，具体 HTTP 错误在 provider 边界转换为字符串上下文

## 快速开始

### 环境要求

- Rust 1.75+（MSRV）
- `direnv`（可选，自动加载环境变量）

### 配置

Provider 配置全部放在 TOML 的 `[provider]` 段(`crates/hermes-cli/hermes.example.toml` 是起点):

```toml
[provider]
kind = "openai"               # openai | anthropic | echo
api_key_env = "OPENAI_API_KEY"  # 运行时只读取这一个环境变量
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
```

运行时只读取 `[provider].api_key_env` 指向的那一个环境变量(默认 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`),其他环境变量(URL、模型、header 名称等)都会被忽略 —— 这些值必须写在 TOML 里。支持 OpenAI 兼容端点(DeepSeek、MiniMax、Ollama、vLLM)只需修改 `base_url` 和 `model`;Anthropic / MiMo 兼容端点改 `api_key_header` 和 `base_url`。

### 构建

```bash
cargo build
```

### 运行 CLI

CLI 启动时会按 `--config` → `~/.perry_hermes/config.toml` → `./hermes.toml` 顺序查找配置。复制示例配置作为起点:

```bash
cp crates/hermes-cli/hermes.example.toml hermes.toml
# 编辑 hermes.toml,填入 API key 指向的 env 变量、model、base_url
cargo run -p hermes-cli
```

离线烟囱测试(用 echo provider 模拟 LLM,不发任何 HTTP 请求):

```bash
echo '[provider]\nkind = "echo"' > hermes.toml
cargo run -p hermes-cli
```

切换模型/端点(也支持 DeepSeek / MiniMax / Ollama / vLLM 等任意 OpenAI 兼容服务):改 `hermes.toml` 里的 `model` 和 `base_url`。

禁用 bash(关闭 `terminal` toolset,只做对话):在 `hermes.toml` 的 `[agent]` 段加 `disabled_toolsets = ["terminal"]`。

调高最大迭代次数(默认 10):在 `hermes.toml` 的 `[agent]` 段加 `max_iterations = 50`。

指定工作目录:用 `--cwd`(只影响该次启动的 `SessionContext`):

```bash
cargo run -p hermes-cli -- --cwd /tmp
```

显式指定配置文件:

```bash
cargo run -p hermes-cli -- --config /path/to/hermes.toml
```

示例 `hermes.toml`：

```toml
[provider]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "mimo-v2.5"
base_url = "https://api.xiaomimimo.com/anthropic/v1"
api_key_header = "api-key"

[provider.thinking]
mode = "off" # off | manual | adaptive
# manual: budget_tokens = 8000
# adaptive: display = "summarized", effort = "medium"

[agent]
max_iterations = 10
disabled_toolsets = []
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
echo '[provider]\nkind = "echo"' > /tmp/hermes-smoke.toml
echo "hello" | cargo run -p hermes-cli --quiet -- --config /tmp/hermes-smoke.toml
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
  ├── anthropic.rs          # Anthropic/MiMo-compatible HTTP + SSE 测试
  └── tool_call_roundtrip.rs  # tool_calls 序列化往返测试

crates/hermes-tools/tests/
  └── bash.rs               # BashTool 实际执行测试
```

项目采用 **TDD 工作流**：RED → GREEN → REFACTOR，严格先写失败测试再写实现代码。
同时会定期删减重复、低信噪比测试，优先保留能守住模块边界和用户可观察行为的测试。

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
| Phase 8 | Anthropic Provider：多 Provider 支持 | ✅ |
| Phase 9 | Skills 加载：SKILL.md 加载 + system prompt 注入 | ✅ |
| Phase 10 | TUI：ratatui 多行编辑器 | |
| Phase 11 | 平台网关：Telegram（grammY-rs） | |
| Phase 12 | Curator：学习循环（Hermes 的灵魂） | |

## 已知问题

- `ToolContext.permissions` 已建模但未强制执行
- 未知 `finish_reason` 会映射为 `FinishReason::Error`，但错误信息仍较粗
- OpenAI-compatible provider 已支持 `Content::Parts` 的 text/image_url content array；其他多媒体 part 类型尚未建模
- OpenAI-compatible provider 会把普通 content 中的 `<think>...</think>` fallback 解析为 reasoning；支持原生 `reasoning_content` 的端点仍优先使用原生字段
- Anthropic provider 支持官方 `x-api-key` header，可通过 TOML `api_key_header = "api-key"` 适配 MiMo 这类 Anthropic-compatible 端点
- Anthropic thinking 默认关闭；如需发送 `thinking` 参数，必须在 TOML 中显式设置 `mode = "manual"` 或 `mode = "adaptive"`。第三方 Anthropic-compatible API 建议先保持 `off`。
- 复用同一 `BashTool` 时 `child.kill().await` 的并发安全未充分测试

## License

MIT
