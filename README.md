# 🦀 Loomis

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024%20edition-orange?style=flat-square&logo=rust" alt="Rust 2024">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License">
  <img src="https://img.shields.io/badge/状态-MVP-green?style=flat-square" alt="Status: MVP">
  <img src="https://img.shields.io/badge/DeepSeek-API-4B6BFB?style=flat-square" alt="DeepSeek API">
</p>

<p align="center">
  <em>一个在终端里运行的 AI 编码助手。<br>
  像和同事结对编程一样，让它读代码、改文件、跑命令、搜索项目。</em>
</p>

<p align="center">
  <img src="UI_screenshot.png" alt="Loomis TUI 截图" width="80%">
</p>

---

## Loomis 能做什么

Loomis 是一个运行在终端里的 AI 编程助手。你通过聊天向它提问，它会**自己动手**——读文件、搜代码、编辑、跑命令——然后给你答案。

| 能力 | 说明 |
|------|------|
| 📖 **读文件** | 让 Loomis 查看任意源文件，它会读取并分析内容 |
| ✏️ **改文件** | 精确编辑，按行替换，不会破坏文件结构 |
| 🔍 **搜索代码** | 用 grep 搜内容、glob 搜文件名，比人更快 |
| 🖥️ **跑命令** | 执行 shell 命令（`cargo build`、`git log`、`npm test`……） |
| 📂 **浏览目录** | `ls` 查看项目结构，了解文件布局 |
| 🧮 **计算器** | 随手算数，支持括号和优先级 |
| 🧵 **子任务委派** | 复杂问题自动拆分成子任务，交给专门的小助手去调查 |

Loomis 有工具，它不只是聊天——它真的能干活。

### 安全沙箱

所有操作都在安全的沙箱里运行：

- **文件访问受限** — 只能在你的项目目录里读写，出不去
- **危险命令被拦截** — `rm -rf /`、`sudo`、`shutdown` 等直接拒绝
- **需要你点头** — 不确定安全的命令会弹窗问你，你可以批准或拒绝
- **所有操作可审计** — 一切记录在 `.loomis/audit.jsonl`

---

## 快速开始

### 你需要

- [Rust](https://www.rust-lang.org/tools/install) 工具链
- [DeepSeek API](https://platform.deepseek.com/) 密钥

### 三步跑起来

```bash
# 1. 克隆并进入项目
git clone https://github.com/Nie-Tianyi/loomis.git
cd loomis

# 2. 把你的 API 密钥写入 .env
echo 'DEEPSEEK_API=sk-your-key-here' > .env

# 3. 启动
cargo run -p loomis --release
```

看到终端界面后，直接打字开始聊天。Loomis 会实时逐字输出回复。

---

## 使用指南

### 基础操作

| 按键 | 功能 |
|------|------|
| `Enter` | 发送消息 |
| `Ctrl+C` | 取消当前操作 / 退出 |
| `Esc` | 取消 |
| `Ctrl+O` | 切换链路追踪调试面板 |
| `PgUp` / `PgDown` | 上下滚动对话 |
| `↑` / `↓` | 浏览历史消息 |
| `←` / `→` / `Home` / `End` | 移动光标 |

### 直接在终端跑命令

在聊天框里以 `!` 开头，命令会立刻执行，结果显示在对话中：

```
!cargo build
!git log --oneline -5
!ls src/
```

命令执行完后，Loomis 可以看到输出并基于它继续对话。

### 斜杠命令

| 命令 | 作用 |
|------|------|
| `/help` | 显示帮助 |
| `/new` | 开始新对话（清空记忆） |
| `/plan` | 切换 Plan Mode（只读研究 & 规划） |
| `/approve` | 批准计划并退出 Plan Mode |
| `/save <名字>` | 保存当前对话 |
| `/resume [名字]` | 恢复历史对话（弹窗选择） |
| `/threads` | 列出所有已保存的对话 |
| `/stats` | 查看对话统计 |
| `/tools` | 列出可用工具 |
| `/debug` | 切换链路追踪调试面板 |
| `/trace-save` | 导出 trace 事件到 JSONL 文件 |
| `/exit` | 退出 |

### 对话持久化

每次 Agent 回复后对话会自动保存到 `.loomis/threads/` 目录。每个对话存两份文件：
- `{名字}.json` — 完整的结构化数据
- `{名字}.md` — 人类可读的 Markdown

下次启动时用 `/resume` 就能接上之前的话题。

### 安全审批

当 Loomis 想执行不在白名单里的命令时，会弹窗问你。用 `↑↓` 选择选项，`Enter` 确认：

```
┌─ 需要你的确认 ───────────────────────────────┐
│  Shell 命令: pip install requests             │
│                                               │
│  ● Approve                                    │
│  ○ Deny                                       │
│  ○ Other: pip install --user requests         │
└───────────────────────────────────────────────┘
```

你可以在 `.loomis/config.toml` 里调整安全策略——哪些命令自动放行、哪些永远拒绝。

### 全链路追踪（Observability）

Loomis 内置了全链路可观测性系统，能够追踪 Agent 内部的所有状态变化：

- **状态栏实时指标** — 底部状态栏显示当前步数、LLM 调用次数、工具调用次数、Token 消耗量
- **调试面板** — 按 `Ctrl+O` 或输入 `/debug` 打开可滚动的 trace 事件列表，查看每一步的详细耗时和资源消耗
- **导出分析** — 输入 `/trace-save` 将所有 trace 事件导出为 JSONL 文件（`.loomis/traces/`），便于离线分析

追踪覆盖的生命周期包括：Agent 运行启停、每个 ReAct 循环步、每次 LLM API 调用（含重试）、每次工具执行、子 Agent 委派等。所有时间数据精确到毫秒级。

### Plan Mode（规划模式）

Plan Mode 让你在动手改代码之前，先让 Loomis 做**只读研究**并写出计划，等你批准后再执行。

**如何工作：**

1. 输入 `/plan` 进入规划模式 — 底部状态栏会显示 `PLAN`
2. Loomis 只能读文件、搜索代码、做调查 — **不能改任何代码**
3. Loomis 会把计划写到 `.loomis/plan.md`（这是它唯一能写的文件）
4. 你查看计划后，输入 `/approve`（或再次 `/plan`）退出规划模式
5. Loomis 恢复完整权限，按计划执行

**被限制的工具：** `edit`、`shell`、`write`（除 plan file 外全部拒绝）

**可用的工具：** `read`、`glob`、`grep`、`ls`、`calculator`、`todo`、`ask_user_question`、`task`（子 Agent，已是只读）

适合复杂的、多步骤的任务——先让 Loomis 研究代码库、设计方案，你审查通过后再让它动手。

---

## 配置

Loomis 的所有行为都可通过 `.loomis/config.toml` 配置。首次运行会自动创建，使用安全的默认值。

```toml
# 示例：让 pip 自动执行，不用每次弹窗
[shell.auto_approve]
prefixes = ["cargo", "git", "python", "pip", "npm", "ls", "cat"]

# 永远拒绝的危险命令（正则表达式）
[shell.deny_patterns]
patterns = ["rm\\s+-rf\\s+(/|~)", "sudo\\s+", "shutdown", "reboot"]

# 文件操作限制
[filesystem]
max_read_bytes = 1_048_576   # 单次读取上限 1 MiB
max_write_bytes = 524_288    # 单次写入上限 512 KiB

# 操作配额
[quotas]
max_steps_per_session = 50   # 单次对话最多 50 轮
max_concurrent_shells = 2    # 最多同时跑 2 个命令
```

完整配置项请参考 [SandboxConfig](libs/tools/src/sandbox/config.rs)。

---

## 测试

```bash
cargo test --all           # 运行所有测试
cargo clippy --all         # 代码检查
```

---

## 给开发者

Loomis 的引擎层是模块化的——9 个独立的 Rust crate 可以被其他 Agent 项目直接复用。

如果你想：
- **实现自己的工具** — 比如接入数据库、网页搜索
- **换成其他 LLM 供应商** — OpenAI、Anthropic、本地模型
- **加入自定义的运行时钩子** — 日志、监控、自定义安全策略
- **深入理解架构原理** — ReAct 循环、流式管道、双层压缩、沙箱纵深防御

请移步开发者指南：

| 文档 | 适合 |
|------|------|
| [**Beginner Developer Guide**](docs/beginner-developer-guide.md) | 第一次写 Agent？跟着教程 10 分钟上手，用 copy-paste 的代码构建你的第一个工具 |
| [**Senior Developer Guide**](docs/senior-developer-guide.md) | 深入架构、Trait 实现、Hook 生命周期、沙箱内部、子 Agent 系统——完整参考手册 |
| [**Sandbox Architecture**](docs/sandbox-architecture.md) | 五层纵深沙箱的设计细节和安全模型 |

---

## 许可证

MIT © 2026
