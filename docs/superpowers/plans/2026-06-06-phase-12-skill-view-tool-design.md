# Phase 12: SkillViewTool 实现计划

## 背景

Phase 9 完成了 skills 加载和系统提示注入，但系统提示块中引用的 `skill_view` 工具尚未实现。当前 LLM 需要回退到用 `bash` 读取 `SKILL.md` 文件。

## 设计目标

1. 实现 `SkillViewTool`，允许 LLM 按名称查看 skill 的完整内容
2. 支持 `category.name` 和纯 `name` 两种调用格式
3. 返回 skill 的 body 内容（不含 frontmatter）
4. 与现有 `BashTool` 保持一致的架构模式

## 实现步骤

### Step 1: 创建工具文件

创建 `crates/hermes-agent/src/tools/skill_view.rs`：

```rust
pub struct SkillViewTool {
    skills_dir: PathBuf,
    cache: Mutex<Option<Vec<Skill>>>,
}
```

- 使用 `skills_dir` 确定 skills 目录位置
- `cache` 缓存已加载的 skills，避免每次调用都重新扫描
- 实现 `Tool` trait，参数为 `skill_name: string`

### Step 2: 注册工具

在 `crates/hermes-agent/src/tool_catalog.rs` 中注册：

```rust
pub fn build_registry(...) -> InMemoryRegistry {
    let mut reg = InMemoryRegistry::new();
    
    if !disabled_toolsets.iter().any(|s| s == "core" || s == "terminal") {
        reg = reg.register(Arc::new(BashTool::new()));
    }
    
    // 新增: 始终注册 SkillViewTool（属于 skills toolset）
    if !disabled_toolsets.iter().any(|s| s == "skills") {
        reg = reg.register(Arc::new(SkillViewTool::new()));
    }
    
    reg
}
```

### Step 3: 添加测试

- 单元测试：参数验证、名称匹配、缓存行为
- 集成测试：模拟 LLM 调用 `skill_view`

## 工具规范

### 参数 schema

```json
{
  "type": "object",
  "properties": {
    "skill_name": {
      "type": "string",
      "description": "Skill name, optionally with category prefix (e.g. 'software-development.subagent-driven-development' or 'subagent-driven-development')"
    }
  },
  "required": ["skill_name"],
  "additionalProperties": false
}
```

### 输出格式

成功时返回 skill body：
```
# subagent-driven-development

## Overview
...

## Usage
...
```

失败时返回清晰错误信息：
```
Skill not found: 'xxx'. Available skills: ...
```

## 错误处理

- `SkillNotFound`: 指定名称的 skill 不存在
- `InvalidFormat`: 参数格式错误
- `ReadError`: 读取文件失败

## 性能考虑

- Skills 缓存在内存中，TTL 5 分钟
- 首次调用时延迟加载
- 使用 `RwLock` 允许并发读取

## 风险与备选

- 如果 skills_dir 不存在，返回空列表而非错误
- 缓存失效策略：基于文件修改时间（可选）
