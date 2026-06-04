# Hermes Python vs Rust 移植 — 实现路径与模块抽象偏差分析

> 调研日期:2026-06-04
> 对照对象:`/Users/amagicpear/.hermes/hermes-agent/` (Hermes Python v0.15.1,~200k+ LOC)
> 对照基线:`/Users/amagicpear/projects/perry_hermes/` (Rust 移植,phase 0 已完成)

## 1. 总结:偏差分级

| 级别 | 含义 | 本次发现 |
|---|---|---|
| 🟢 **对齐** | 抽象边界一致,Rust 版本用类型系统比 Python 更严格 | 7 处 |
| 🟡 **简化** | Rust 版本砍掉了 Python 的某些能力,phase 0 范围内可接受 | 5 处 |
| 🔴 **缺失** | Hermes 有明确概念但 Rust 版本完全没建模 | 3 处 |
| 🟣 **新设计** | Rust 版本主动引入 Python 没有的概念 | 4 处 |

## 2. 核心抽象对照表

### 2.1 三 trait 对照

| 我们的设计 (Rust) | Hermes (Python) | 评估 |
|---|---|---|
| `Provider` trait(`crates/hermes-core/src/provider.rs`) | 没有 trait。`AIAgent` 在 init 时直接 import `OpenAI` SDK,选 backend 靠 if/elif 链 | 🟣 新设计 — 更干净,Rust 多态 vs Python 鸭子类型 |
| `Tool` trait(`crates/hermes-core/src/tool.rs`) | 模块级 `registry.register(name, schema, handler, ...)` 单例 + auto-import | 🟡 简化 — 失去 "drop a file in tools/,auto-discovered" 行为,但 Rust 的显式 model 不会导致隐藏副作用 |
| `ToolRegistry` trait + `InMemoryRegistry` | 没有 trait。`tools/registry.py` 是 module-level singleton,所有工具通过全局 `registry` 对象访问 | 🟣 新设计 — 多 registry(测试隔离、子 agent 隔离)成为可能 |

### 2.2 类型对照

| 我们的 `Message` | Hermes 内部消息 | 评估 |
|---|---|---|
| `role: Role { System, User, Assistant, Tool }` | `{"role": "system/user/assistant/tool", ...}` OpenAI 格式 | 🟢 对齐 |
| `content: Content`(untagged enum,text 或 Parts) | 同上 | 🟢 对齐 |
| `reasoning: Option<String>` 在 message 上 | `assistant_msg["reasoning"]` 也在 message 上(AGENTS.md 强调"不要做错") | 🟢 对齐 — 设计文档已经吸取了 Hermes 的教训 |
| `tool_call_id: Option<String>` | 同样 | 🟢 对齐 |
| `tool_calls: Option<Vec<ToolCall>>` | 同样 | 🟢 对齐 |

### 2.3 错误对照

| 我们的设计 | Hermes | 评估 |
|---|---|---|
| `ProviderError { RateLimited{retry_after_secs}, ContextLengthExceeded, Auth, Transport, InvalidResponse, Cancelled, Other }` | 异常 + `agent/error_classifier.py` 里的 `FailoverReason` 枚举 | 🟣 新设计 — Rust 用 `thiserror` 强类型 enum 比 Python 的字符串 + 分类器更安全 |
| `ToolError { NotFound, InvalidArgs, Execution, Permission, Cancelled, Timeout }` | 工具自己 raise,被 `model_tools.handle_function_call` 包成 JSON 字符串返回 | 🟣 新设计 — Rust 把 cancel/timeout 显式化,Python 依赖 GIL |
| `LoopError { MaxIterations, Timeout, Cancelled, ContentFilter, Provider, Compression }` | 循环内 `break` + 设标志位 | 🟣 新设计 |

### 2.4 取消机制对照

| 我们的设计 | Hermes | 评估 |
|---|---|---|
| `CancellationToken` 贯穿每个 async 方法,`tokio::select!` 监听 | `_interrupt_requested: bool` 标志位,Python GIL 偶然确保 Ctrl-C 能送达 | 🟣 新设计 — 文档 §8 第 1 条正确指出了这一点 |

## 3. Hermes 独有但我们没建模的概念

### 3.1 🔴 Toolset 概念(完全缺失)

Hermes 的核心组织单位是 **toolset**(`toolsets.py` 的 `TOOLSETS` dict):
- 每个工具属于一个 toolset(`"terminal"`, `"messaging"`, `"browser"`, `"mcp"`, …)
- 平台选择 base toolset(Telegram 用 `"messaging"`,CLI 用全量)
- `_HERMES_CORE_TOOLS` 是默认 bundle,所有 platform 继承
- 启用/禁用通过 `enabled_toolsets` / `disabled_toolsets` config

```python
# toolsets.py 的核心结构
TOOLSETS = {
    "terminal": {...},
    "messaging": {...},
    "browser": {...},
    ...
}
_HERMES_CORE_TOOLS = [...]  # 默认 bundle
```

我们的 `Tool` trait **完全没有 toolset 字段**。`InMemoryRegistry` 是平铺的。

**为什么重要**:Hermes 的平台抽象(messaging / CLI / cron / kanban)全靠 toolset 实现。如果我们要做 hermes-gateway(phase 11),没有 toolset 概念,就要么每个 platform 重复 `InMemoryRegistry.register(...)`、要么发明新概念。

**建议**:在 `Tool` trait 加 `fn toolset(&self) -> &'static str` 字段;在 `ToolRegistry` 加 `toolsets() -> HashMap<&'static str, Vec<&str>>` 方法;在 `LoopConfig` 加 `enabled_toolsets: Option<Vec<String>>`。

### 3.2 🔴 Tool 元数据(完全缺失)

Hermes 的 `registry.register()` 接受大量元数据:
```python
registry.register(
    name="terminal",
    toolset="terminal",
    schema={"name": "terminal", "description": "...", "parameters": {...}},
    handler=lambda args, **kw: ...,
    check_fn=lambda: bool(os.getenv("EXAMPLE_API_KEY")),  # 可用性检查
    requires_env=["EXAMPLE_API_KEY"],
    emoji="⚡",  # 显示提示
)
```

我们的 `Tool` trait 只有 `name` / `description` / `parameters_schema` / `execute`。**没有**:
- `check_fn: fn() -> bool` — 用于"这个工具现在能用吗?需要 API key 吗?"
- `requires_env: &[&str]` — 配置缺失时给用户友好提示
- `emoji: Option<&str>` — 显示
- `toolset` 字段(见上)

**影响**:`hermes-cli` 第 4 阶段需要 `hermes tools` 子命令列出可用工具、显示 ✗/✓ 状态 — 这就要求 `check_fn`。`display.py::get_tool_emoji()` 同理。

**建议**:在 `Tool` trait 加:
```rust
fn check(&self) -> Result<(), ToolCheckError> { Ok(()) }  // 默认永远可用
fn emoji(&self) -> Option<&str> { None }
fn requires_env(&self) -> &[&str] { &[] }
```

### 3.3 🟡 IterationBudget 过于简陋

Hermes 的 `agent/iteration_budget.py`:
```python
class IterationBudget:
    """Thread-safe iteration counter for an agent."""
    def __init__(self, max_total: int): ...
    def consume(self) -> bool: ...   # 原子 +1,满了返回 False
    def refund(self) -> None: ...    # 用 execute_code 编程调用时退回
    @property
    def used(self) -> int: ...
    @property
    def remaining(self) -> int: ...
```

关键能力:
- **refund** — 程序化工具调用(`execute_code`)不算 iteration
- **每个 subagent 独立 budget** — parent 90,subagent 50(从 `delegation.max_iterations`)
- **grace call** — budget 用完后允许再调一次 LLM 来"收尾"
- **thread-safe** — subagent 在另一线程跑

我们 `LoopConfig.max_iterations: u32` 是个字段,不是对象。**无法**:
- 表达 refund 语义
- 在 delegation 场景给 subagent 独立预算
- 表达 grace call

**建议**:phase 1 写 `AgentLoop` 时,把 `LoopConfig.max_iterations` 升级为 `LoopConfig.budget: Arc<IterationBudget>`,或者在 `AgentLoop` 上加 `pub fn with_budget(budget: Arc<IterationBudget>) -> Self`。budget 至少要有 `consume()` / `refund()` 方法。

**优先级**:中。phase 0–3 用 `u32` 够,phase 11+(delegation)会需要。

## 4. Hermes 有但我们在 phase 0 故意推迟的概念

这些都在设计文档的 phase 5+ 路线图里,偏差是有意为之:

| 概念 | Hermes 文件 | 我们的 phase | 评估 |
|---|---|---|---|
| 上下文压缩 | `agent/context_compressor.py` (2078 行) + `conversation_compression.py` (758 行) | phase 7 | 🟡 简化 — 我们只有 `LoopError::Compression` 变体,没有 `ContextCompressor` trait |
| Skills | `skills/`、`optional-skills/`、`tools/skills_hub.py`、`agent/skill_commands.py` | phase 9 | 🟡 简化 — 设计文档只规划了 "load .md files as system-prompt content",缺失 SKILL.md frontmatter、tag、category、激活机制 |
| 内存/Memory Provider | `agent/memory_provider.py` + `plugins/memory/{honcho,mem0,...}` | 远期 | 🟡 简化 — 整体缺失 |
| Session DB | `hermes_state.py` SQLite + FTS5 | phase 4+ 需要时再加 | 🟡 简化 — `RunResult.messages` 已经把数据给我们了,只是没存 |
| Curator | `agent/curator.py` (1843 行) + `curator_backup.py` | phase 12 | 🟡 简化 |
| Plugins | `hermes_cli/plugins.py` + `plugins/<name>/` | 远期 | 🟡 简化 |
| Skin 引擎 | `hermes_cli/skin_engine.py` | 远期 | 🟡 简化 |
| ACP adapter | `acp_adapter/` | 远期 | 🟡 简化 |
| 17 messaging platforms | `gateway/platforms/` | phase 11+ | 🟡 简化 |
| Cron | `cron/jobs.py` + `scheduler.py` | 远期 | 🟡 简化 |
| Kanban | `plugins/kanban/` | 远期 | 🟡 简化 |
| Prompt caching | `agent/prompt_caching.py` | 远期 | 🟡 简化 |
| Checkpointing | AIAgent 的 `checkpoints_enabled` 标志 | 远期 | 🟡 简化 |
| Fallback model | AIAgent 的 `fallback_model` 字段 | 远期 | 🟡 简化 |
| Credential pool | `agent/credential_pool.py` (2183 行) | 远期 | 🟡 简化 |

## 5. Rust 版本主动引入的 Python 没有的概念(优势)

| 概念 | 设计意图 | 评估 |
|---|---|---|
| `Send + Sync` bounds on traits | 允许 `Arc<dyn Provider>` 跨线程,子 agent 在 `tokio::spawn` 里跑 | 🟢 Rust 独有优势 |
| `CancellationToken` 贯穿 | 替代 Python 偶然能用的 GIL 中断 | 🟢 设计文档 §8 显式承认这是 Rust 设计的核心动机 |
| `FinishReason` 5 态 enum | Python 隐式用 `response.tool_calls is not None` 区分 | 🟢 强类型更安全 |
| `ToolContext { session_id, working_dir, permissions }` | Python 用 process-global + 隐式 cwd | 🟢 显式上下文,容易测试 |
| `Attachment` / `ContentPart` 多模态 | Python 通过 sanitize_strip_images 走 fallback 路径 | 🟢 v0 不急,但 v0 就建模是对的 |

## 6. 实现路径对照

| 阶段 | 我们的 roadmap | Hermes 实际历史 | 偏差 |
|---|---|---|---|
| 0 — Skeleton | 8 crate stub + traits | 早期 monolith(单 `run_agent.py`) | 🟢 类似(我们拆分更早) |
| 1 — Echo loop | Mock provider,loop 跑 1 次 | Hermes 早期也是先 mock 测试 | 🟢 对齐 |
| 2 — OpenAI | 写 OpenAI provider | Hermes 早期只支持 OpenAI | 🟢 对齐 |
| 3 — Bash tool | BashTool | Hermes 早期 `terminal_tool.py` | 🟢 对齐 |
| 4 — CLI | REPL | HermesCLI = `cli.py` (现在 11k+ LOC) | 🟡 我们低估了 CLI 的复杂度,实际需要更长时间 |
| 5–7 — Streaming, Interrupt, Compression | 单独 phase | Hermes 早期一次性引入,迭代成熟 | 🟡 Hermes 把这些"基本功"做得更深 |
| 8 — Anthropic provider | 单独 phase | Hermes 有专门 adapter 文件 (2303 行 `anthropic_adapter.py`) | 🟢 对齐 |
| 9 — Skills | "load .md files" | Hermes 有完整 skills 生态(`SKILL.md` frontmatter、`~/.hermes/skills/`、`optional-skills/`、自动激活、usage tracking、curator 自动归档) | 🔴 **设计文档严重低估** — phase 9 的工作量至少是其他 phase 的 3 倍 |
| 10 — TUI | ratatui | Hermes 有完整 Ink (React) TUI + tui_gateway JSON-RPC 进程 | 🟡 范围对齐,但实现方式不同 |
| 11 — First platform | Telegram via `grammY-rs` | Hermes 有 17 个 platform | 🟢 对齐,选 1 个先做是对的 |
| 12 — Curator | "learning loop" | Hermes 的 curator 包含:auto-review LLM 调用、stale-after-days、auto-archive、backup、rollback、CLI verbs | 🟡 我们只画了 trait,实际工作量远大于 2000 LOC |

**特别注意 phase 9 (Skills)** — Hermes 把 Skills 做成了一等公民:
- `skills/<category>/<name>/SKILL.md` + frontmatter(`name`, `description`, `version`, `platforms`, `tags`, `category`, `config` 依赖)
- `optional-skills/` — 重型 / 利基 skill 不默认激活
- `scan_skill_commands()` — skill 自带 slash command
- `skill_manage(action="delete"/"create"/...)` — agent 可以管理 skill
- `agent/skill_preprocessing.py` — 加载时预处理
- `skill_usage.py` + curator — usage tracking 和 auto-archive

我们的 phase 9 只规划"load .md files as system-prompt content"。这个范围可能需要拆成 phase 9a / 9b / 9c。

## 7. 优先级调整建议

如果按偏差严重度排,**实际落地时建议这样排序**:

| 优先级 | 偏差 | 推荐时机 |
|---|---|---|
| **P0** | toolset 概念缺失 | phase 4(CLI 需要 `--tools` flag 时) |
| **P0** | tool 元数据(`check_fn` / `emoji` / `requires_env`) | phase 4(CLI `hermes tools` 命令) |
| **P1** | `IterationBudget` 升级为对象 | phase 1(写 `AgentLoop` 时直接做) |
| **P1** | `ContextCompressor` trait | phase 7 之前引入 trait,phase 7 写实现 |
| **P1** | `Session` / `SessionStore` 概念 | phase 4 后半(CLI 想保留历史) |
| **P2** | skills 范围重估 | phase 9 之前重新设计 |
| **P2** | curator 范围重估 | phase 12 之前重新设计 |
| **P3** | `enabled_toolsets` config | phase 11(第一个 platform) |

## 8. 与设计文档原文的偏差

我们 phase 0 的实现**严格遵循**了设计文档第 2、3、5、7 节。所以本文的偏差分析有两层:

1. **设计文档本身的偏差**(与 Hermes Python 的真实结构相比) — 见上 P0/P1 的内容
2. **我们当前实现 vs 设计文档** — 零偏差,全部按 doc 实施

**最值得回头更新设计文档的点**:
- §3.2 `Tool` trait 缺 `toolset` / `check_fn` / `emoji` / `requires_env` 字段
- §3.3 `ToolRegistry` 缺 `by_toolset()` 视图
- §4 `LoopConfig` 用 `max_iterations: u32`,应该升级为 `budget: IterationBudget`
- §6 路线图 phase 9 (Skills) 的范围严重低估
- §6 路线图 phase 12 (Curator) 的范围低估
- §6 路线图 phase 4 (CLI) 的工作量可能也比 ~500 LOC 大,实际 HermesCLI ~11k LOC

## 9. 资源链接

- Hermes Python 源码: `/Users/amagicpear/.hermes/hermes-agent/`
- 关键文件:
  - `run_agent.py` (5115 行) — AIAgent 类,60+ 构造参数
  - `model_tools.py` (1174 行) — `get_tool_definitions()` + `handle_function_call()`
  - `toolsets.py` (882 行) — toolset 字典
  - `agent/conversation_loop.py` (4836 行) — 抽离的循环
  - `agent/display.py` (1033 行) — 纯展示(spinner、emoji、preview)
  - `agent/iteration_budget.py` (62 行) — 线程安全 budget 对象
  - `AGENTS.md` (1172 行) — Hermes 自己的架构文档
  - `tools/registry.py` — 全局 tool registry singleton
