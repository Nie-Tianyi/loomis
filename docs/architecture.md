Rust 模块化 Agent 框架架构设计 (极简实用版)基于“务实工程”与“胖模型，瘦框架”的设计理念，拒绝过早优化与过度设计。本架构采用极简的 ReAct (Reason + Act) 主循环，且在初期直接提供专为 Coding 场景优化的具体 Memory 实现，确保项目能以最快速度落地且具备实战能力。
flowchart TD
    %% 样式定义
    classDef layer fill:#f9f9f9,stroke:#333,stroke-width:2px,color:#333, rx:5px, ry:5px;
    classDef core fill:#e1f5fe,stroke:#0288d1,stroke-width:2px,color:#000;
    
    %% UI 交互层
    subgraph UI_Layer ["5. UI 交互层 (agent-ui)"]
        direction LR
        CLI[命令行/TUI]
    end
    
    %% 业务应用层
    subgraph App_Layer ["4. 业务应用层 (agent-app)"]
        direction LR
        CodingAgent[Coding Agent]
    end

    %% 核心引擎层
    subgraph Engine_Layer ["3. 核心编排引擎 (agent-engine)"]
        direction TB
        Runner[AgentRunner\n核心 ReAct 循环]
        Hooks[Hooks 事件中间件]
        
        Runner -. 触发生命周期 .-> Hooks
    end

    %% 能力层 (去除抽象，直接提供实战组件)
    subgraph Capability_Layer ["2. 能力与存储层"]
        direction LR
        Tools["工具集 (agent-tools)\nTraits, 过程宏解析"]
        Memory["记忆存储 (agent-memory)\nFileSystemMemory\n(专为代码场景优化)"]
    end

    %% 基础设施层
    subgraph Provider_Layer ["1. 底层模型接入 (agent-provider)"]
        direction LR
        LLM[LLM Client]
    end

    %% 依赖与流转关系
    UI_Layer == 异步事件流 ==> App_Layer
    App_Layer == 装配 & 启动 ==> Engine_Layer
    
    Engine_Layer == 循环调用 ==> Capability_Layer
    Engine_Layer == 请求模型 ==> Provider_Layer
    
    Capability_Layer == 依赖基础模型 ==> Provider_Layer
    
    %% 应用样式
    class UI_Layer,App_Layer,Capability_Layer,Provider_Layer layer;
    class Engine_Layer core;
1. 整体 Workspace 结构由于去除了非必要的抽象和多余插件，我们的工程结构变得非常清爽：my_agent_workspace/
├── Cargo.toml                  # Workspace 根配置
├── crates/
│   ├── agent-provider/         # 底层模型接入 (目前仅需 LLM)
│   ├── agent-memory/           # 基于文件系统的具体记忆实现
│   ├── agent-tools/            # 工具集抽象与宏 (Tool Trait, Schema 生成)
│   ├── agent-engine/           # 核心编排引擎 (ReAct 循环, Hooks)
│   ├── agent-app/              # 业务 Agent 组装层
│   └── agent-ui/               # 交互展示层 (CLI)
2. 核心层级详细设计与 Trait 签名2.1 底层模型层 agent-provider目标: 抹平大模型厂商的 API 差异，核心是支持原生的 Tool Calling。use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role, // User, Assistant, System, Tool
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>, // 当 role 为 Tool 时使用
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

pub trait LLMClient: Send + Sync {
    async fn generate(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError>;
    async fn stream(&self, req: CompletionRequest) -> Result<BoxStream<'_, Result<StreamChunk, ProviderError>>, ProviderError>;
}
2.2 工具与能力层 agent-tools目标: 提供标准化的函数调用能力。pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> serde_json::Value;
    
    /// 执行逻辑，传入 JSON，传出 String (观测结果 Observation)
    async fn execute(&self, args: serde_json::Value) -> Result<String, ToolError>;
}
2.3 记忆与存储层 agent-memory (拒绝过度设计，直接提供具体实现)目标: 不搞 Trait 抽象，直接打造一个专为 Coding 场景设计的强大内存管理器，一切方法直接写在 FileSystemMemory 上。use std::path::PathBuf;

pub struct FileSystemMemory {
    base_dir: PathBuf,
}

impl FileSystemMemory {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { base_dir: path.into() }
    }

    // --- 1. 基础读写能力 (供引擎使用) ---
    pub async fn add_message(&self, session_id: &str, msg: Message) -> Result<(), MemoryError> {
        // 直接将 Message 追加到本地 JSONL 或 Markdown 文件中
        Ok(())
    }

    pub async fn get_context(&self, session_id: &str, max_tokens: Option<usize>) -> Result<Vec<Message>, MemoryError> {
        // 从本地文件读取解析
        Ok(vec![])
    }

    // --- 2. 专为 Coding Agent 设计的高阶能力 (供专属 Tool 使用) ---
    
    pub async fn micro_compact(&self, session_id: &str) -> Result<(), MemoryError> { Ok(()) }
    pub async fn manual_compact(&self, session_id: &str, llm: &dyn LLMClient) -> Result<(), MemoryError> { Ok(()) }
}
2.4 核心编排引擎层 agent-engine 与 Hooks 设计目标: 定义清晰的生命周期 Hook 接口。引擎通过遍历 Hooks 列表，让外部代码能够在不侵入 AgentRunner 的情况下感知和干预运行状态。use std::collections::HashMap;
use std::sync::Arc;

// ==========================================
// Hook 接口定义
// ==========================================
#[allow(unused_variables)]
pub trait AgentHook: Send + Sync {
    /// Agent 刚接收到用户输入，开始一次完整任务时触发
    async fn on_run_start(&self, session_id: &str, user_input: &str) {}
    
    /// 准备向 LLM 发送请求前触发
    async fn on_llm_start(&self, session_id: &str) {}
    
    /// LLM 返回结果后触发
    async fn on_llm_end(&self, session_id: &str, response: &Message) {}
    
    /// 准备执行具体的工具前触发。
    /// 注意：这里返回 Result。如果 Hook 返回 Error（比如人工拒绝执行），引擎将中断！
    async fn before_tool_call(&self, session_id: &str, tool_call: &ToolCall) -> Result<(), EngineError> { 
        Ok(()) 
    }
    
    /// 工具执行完毕后触发，可以获取执行结果 observation
    async fn after_tool_call(&self, session_id: &str, tool_call: &ToolCall, observation: &str) {}
}

// ==========================================
// 引擎 Context 与 Runner
// ==========================================
pub struct EngineContext {
    pub llm: Box<dyn LLMClient>,
    pub memory: Arc<FileSystemMemory>, 
    pub tools: HashMap<String, Box<dyn Tool>>,
    pub hooks: Vec<Box<dyn AgentHook>>,
}

pub struct AgentRunner {
    ctx: EngineContext,
}

impl AgentRunner {
    pub fn new(ctx: EngineContext) -> Self { Self { ctx } }

    /// 核心 ReAct 循环
    pub async fn run(&self, session_id: &str, user_input: &str) -> Result<String, EngineError> {
        // [触发 Hook] 任务开始
        for hook in &self.ctx.hooks { hook.on_run_start(session_id, user_input).await; }

        self.ctx.memory.add_message(session_id, Message { /*...*/ }).await?;
        
        let mut steps = 0;
        loop {
            if steps >= 15 { return Err(EngineError::MaxStepsExceeded); }
            steps += 1;

            let messages = self.ctx.memory.get_context(session_id, None).await?;
            
            // [触发 Hook] LLM 思考开始
            for hook in &self.ctx.hooks { hook.on_llm_start(session_id).await; }
            
            let response = self.ctx.llm.generate(/*...*/).await?;
            
            // [触发 Hook] LLM 思考结束
            for hook in &self.ctx.hooks { hook.on_llm_end(session_id, &response.message).await; }
            
            self.ctx.memory.add_message(session_id, response.message.clone()).await?;

            // 检查并执行 Tool
            if let Some(tool_calls) = response.message.tool_calls {
                if tool_calls.is_empty() { return Ok(response.message.content); }

                for tool_call in tool_calls {
                    let tool = self.ctx.tools.get(&tool_call.name).unwrap();

                    // [触发 Hook] 执行前，如果 Hook 报错则直接向外抛出中断循环
                    for hook in &self.ctx.hooks { 
                        hook.before_tool_call(session_id, &tool_call).await?; 
                    }

                    // 执行工具
                    let observation = tool.execute(tool_call.arguments.clone()).await.unwrap_or_else(|e| e.to_string());

                    // [触发 Hook] 执行后
                    for hook in &self.ctx.hooks { 
                        hook.after_tool_call(session_id, &tool_call, &observation).await; 
                    }
                    
                    self.ctx.memory.add_message(session_id, Message { /*...*/ }).await?;
                }
            } else {
                return Ok(response.message.content);
            }
        }
    }
}
2.5 业务应用层 agent-app (装配极其简单)use std::sync::Arc;
use serde_json::Value;

pub struct MemoryCompactTool { fs_memory: Arc<FileSystemMemory> }
impl Tool for MemoryCompactTool { /* ... */ }

pub fn build_coding_agent(work_dir: &str) -> AgentRunner {
    // ... 初始化 LLM, Memory, Tools ...
    
    let ctx = EngineContext {
        llm: Box::new(OpenAIClient::new("claude-3-5-sonnet")),
        memory: Arc::new(FileSystemMemory::new(work_dir)),
        tools: HashMap::new(),
        // 在这里注入你想要的 Hooks！见下文 2.6
        hooks: vec![
            Box::new(CliLoggerHook),
            Box::new(DangerousCommandApprovalHook),
        ],
    };
    AgentRunner::new(ctx)
}
2.6 Hooks 典型实战案例 (极其强大)利用 AgentHook，我们可以在不改变一行引擎代码的情况下，实现极其丰富的功能。实战案例 1：终端进度与日志展示 (CLI Logger)专为命令行打造，在不同的生命周期打印好看的进度条或日志。pub struct CliLoggerHook;

impl AgentHook for CliLoggerHook {
    async fn on_llm_start(&self, _session_id: &str) {
        // 利用类似 indicatif 库展示转圈动画
        println!("⏳ Agent 正在思考中...");
    }

    async fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), EngineError> {
        println!("🔧 决定执行工具: {} | 参数: {}", tool.name, tool.arguments);
        Ok(())
    }
}
实战案例 2：危险操作的人工拦截 (Human in the loop)像 Claude Code 一样，遇到高危命令要求用户输入 Y/n。pub struct DangerousCommandApprovalHook;

impl AgentHook for DangerousCommandApprovalHook {
    async fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), EngineError> {
        if tool.name == "bash_execute" {
            // 解析出将要执行的命令
            let cmd = tool.arguments.get("command").unwrap().as_str().unwrap();
            
            // 如果包含敏感词
            if cmd.contains("rm -rf") || cmd.contains("drop table") {
                println!("⚠️ 警告：Agent 试图执行高危命令: \n> {}\n允许执行吗？(y/N)", cmd);
                
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).unwrap();
                
                if input.trim().to_lowercase() != "y" {
                    // 返回 Error 强制中断引擎执行！
                    return Err(EngineError::HumanRejectedTool(tool.name.clone()));
                }
            }
        }
        Ok(())
    }
}
实战案例 3：对接 Web 前端 (UI Event Stream)当你后续想把这个 Agent 做成 GUI (Tauri) 或 Web 服务时，只需要写一个负责发事件的 Hook。use tokio::sync::mpsc;

// 定义发给前端的事件枚举
pub enum UiEvent {
    Thinking,
    ToolCalled(String),
    Finished(String),
}

pub struct UiStreamHook {
    // 注入一个 MPSC 发送端
    pub tx: mpsc::Sender<UiEvent>,
}

impl AgentHook for UiStreamHook {
    async fn on_llm_start(&self, _session_id: &str) {
        let _ = self.tx.send(UiEvent::Thinking).await;
    }
    async fn before_tool_call(&self, _session_id: &str, tool: &ToolCall) -> Result<(), EngineError> {
        let _ = self.tx.send(UiEvent::ToolCalled(tool.name.clone())).await;
        Ok(())
    }
}
