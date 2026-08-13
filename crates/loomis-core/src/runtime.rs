//! Agent runtime — UI-facing façade over the assembled agent.
//!
//! The [`Runtime`] owns the agent, its shared state, and the background
//! driver task. Frontends send [`RuntimeCommand`]s, consume the
//! [`AgentEvent`](crate::AgentEvent) stream, and call the sync façade
//! methods for quick operations (save/resume/skills/plan mode/…).
//!
//! ## Channel topology
//!
//! ```text
//! UI thread                            Driver task (tokio::spawn)
//! ─────────                            ────────────────────────
//! Runtime::send ── RuntimeCommand ──→  cmd_rx
//! agent_rx ←────── AgentEvent ──────── agent_tx (agent loop + SandboxHook + user cmds)
//! ```
//!
//! Everything blocking (agent loop, interventions, shell commands) happens
//! inside the driver task, so a frontend's event loop never stalls.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use agent_oxide::deepseek::DeepSeekClient;
use agent_oxide::engine::{Agent, AgentEvent, CallOrigin, ResponseRouter};
use agent_oxide::memory::{Memory, PendingHints, SharedMemory};
use agent_oxide::observability::TraceStore;
use agent_oxide::persistence::PersistenceConfig;
use agent_oxide::provider::{Message, Role};
use agent_oxide::sandbox::shell_filter::{CommandVerdict, ShellFilter};
use agent_oxide::skills::{ActiveSkills, SkillRegistry};
use futures_util::FutureExt;
use tokio::sync::mpsc;

use crate::app::{self, AgentKit};
use crate::config::CoreConfig;
use crate::hooks::{PlanModeState, SYSPROMPT_MARKER, insert_before_history};
use crate::tools::{TodoItem, archive_plan};
use crate::user_shell::execute_shell_command;
use crate::{InterventionResponse, ThreadInfo};

// ── Commands ───────────────────────────────────────────────────────────────

/// Commands sent from the UI thread to the agent driver task.
#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    /// User submitted a message — push to memory and run the agent loop.
    RunAgent(String),
    /// User typed !command — execute shell command asynchronously.
    RunShell(String),
    /// Cancel the currently-running generation.
    CancelGeneration,
    /// Reset conversation, preserving system prompt.
    ClearConversation,
    /// User responded to an intervention prompt.
    InterventionResponse {
        request_id: String,
        response: InterventionResponse,
    },
    /// Signal the driver task to exit.
    Shutdown,
}

// ── Runtime ─────────────────────────────────────────────────────────────────

/// Shared handle to a fully-assembled agent runtime. Cheap to clone
/// (`Arc` inside) — a frontend can hold one in its event loop and hand
/// clones to handlers (e.g. a web server's request handlers).
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

/// Internal state shared between the runtime handle and the driver task.
struct RuntimeInner {
    agent: Arc<Agent<DeepSeekClient>>,
    memory: SharedMemory,
    tool_names: Vec<String>,
    model: String,
    /// Sending half of the agent-event channel — the agent loop, sandbox
    /// hook, and driver all emit through it.
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    /// Sending half of the command channel — frontends send through it.
    cmd_tx: mpsc::UnboundedSender<RuntimeCommand>,
    /// Receiving halves handed to the driver on [`Runtime::spawn`].
    /// Wrapped in `Option` so the event stream has exactly one consumer.
    rx_pair: Mutex<
        Option<(
            mpsc::UnboundedReceiver<RuntimeCommand>,
            mpsc::UnboundedReceiver<AgentEvent>,
        )>,
    >,
    /// Routes intervention responses to the correct requester
    /// (SandboxHook, AskUserQuestionTool, …).
    response_router: Arc<ResponseRouter>,
    /// Queue for user hints injected during active agent runs.
    /// Drained by the agent loop before each LLM call.
    pending_hints: PendingHints,
    /// Persistence config — directory layout and naming for thread storage.
    persistence_config: PersistenceConfig,
    /// Shared todo list state — written by [`TodoTool`](crate::tools::TodoTool),
    /// read by frontends.
    todos: Arc<RwLock<Vec<TodoItem>>>,
    /// Shared trace store — written by ObservabilityHook, read by frontends.
    trace_store: Arc<TraceStore>,
    /// Shared plan-mode toggle between frontend and PlanModeHook.
    plan_mode: Arc<PlanModeState>,
    /// Directory where approved plans are archived (`.loomis/plan/`).
    plan_dir: PathBuf,
    /// Discovered skills — read-only after startup.
    skill_registry: Arc<SkillRegistry>,
    /// Currently active skills — written by SkillTool, read by SkillHook.
    active_skills: ActiveSkills,
    /// Shell-command policy — the same instance backing SandboxHook,
    /// reused to classify user `!command` invocations.
    shell_filter: ShellFilter,
    /// Shell execution chain — runs user `!command` invocations after
    /// [`shell_filter`](Self::shell_filter) classification.
    shell_runner: agent_oxide::sandbox::ShellRunner,
    /// Workspace root — all file operations are relative to it.
    workspace_root: PathBuf,
}

/// Cheap per-frame read handles for a frontend. All fields are `Arc`
/// clones — acquiring them never allocates or blocks.
pub struct UiState {
    pub model: String,
    pub memory: SharedMemory,
    pub tool_names: Vec<String>,
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    pub pending_hints: PendingHints,
    pub persistence_config: PersistenceConfig,
    pub trace_store: Arc<TraceStore>,
    pub plan_mode: Arc<PlanModeState>,
    pub plan_dir: PathBuf,
    pub skill_registry: Arc<SkillRegistry>,
    pub active_skills: ActiveSkills,
    pub shell_filter: ShellFilter,
}

/// Outcome of approving the plan.
pub enum ApproveOutcome {
    /// Plan mode was not active — nothing to approve.
    NotInPlanMode,
    /// Plan mode deactivated. `archive` is the archived path, the archiving
    /// error, or `Ok(None)` when the plan file was empty.
    Approved {
        archive: Result<Option<PathBuf>, String>,
    },
}

/// Failure while assembling the runtime.
#[derive(Debug)]
pub enum BuildError {
    /// Workspace filesystem creation failed (root missing or unusable).
    Workspace(agent_oxide::sandbox::FsError),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Workspace(e) => write!(f, "Cannot create workspace: {e}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl Runtime {
    /// Assemble the agent (tools, hooks, sandbox, channels). Synchronous —
    /// only [`Runtime::spawn`] requires a live tokio runtime.
    pub fn build(config: CoreConfig) -> Result<Self, BuildError> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<RuntimeCommand>();
        let kit: AgentKit = app::assemble(
            &config.api_key,
            &config.workspace_root,
            &config.model,
            &config.flash_model,
            &config.resolve_sandbox(),
            &config.persistence,
        )?;
        let AgentKit {
            agent,
            memory,
            tool_names,
            model,
            agent_rx,
            agent_tx,
            response_router,
            pending_hints,
            persistence_config,
            todos,
            trace_store,
            plan_mode,
            plan_dir,
            skill_registry,
            active_skills,
            shell_filter,
            shell_runner,
        } = kit;

        // Fresh session: reset the current-thread marker so PersistenceHook's
        // on_run_start treats the first query as a new conversation and
        // generates a title for it. (A leftover name from a previous session
        // must not hijack the new conversation.)
        let _ = agent_oxide::persistence::write_current_thread_name(
            &persistence_config.default_thread_name,
            &config.workspace_root,
            &persistence_config,
        );

        Ok(Self {
            inner: Arc::new(RuntimeInner {
                agent: Arc::new(agent),
                memory,
                tool_names,
                model,
                agent_tx,
                cmd_tx,
                rx_pair: Mutex::new(Some((cmd_rx, agent_rx))),
                response_router,
                pending_hints,
                persistence_config,
                todos,
                trace_store,
                plan_mode,
                plan_dir,
                skill_registry,
                active_skills,
                shell_filter,
                shell_runner,
                workspace_root: config.workspace_root,
            }),
        })
    }

    /// Start the driver task and return the agent-event stream.
    ///
    /// Must be called from inside a tokio runtime. The stream has a single
    /// consumer — calling `spawn` twice panics.
    pub fn spawn(&self) -> mpsc::UnboundedReceiver<AgentEvent> {
        let mut slot = self
            .inner
            .rx_pair
            .lock()
            .expect("runtime receiver lock poisoned");
        let (cmd_rx, agent_rx) = slot.take().expect(
            "Runtime::spawn called more than once — the event stream has a single consumer",
        );
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move { driver(inner, cmd_rx).await });
        agent_rx
    }

    /// Cheap per-frame read handles.
    pub fn ui(&self) -> UiState {
        UiState {
            model: self.inner.model.clone(),
            memory: self.inner.memory.clone(),
            tool_names: self.inner.tool_names.clone(),
            todos: self.inner.todos.clone(),
            pending_hints: self.inner.pending_hints.clone(),
            persistence_config: self.inner.persistence_config.clone(),
            trace_store: self.inner.trace_store.clone(),
            plan_mode: self.inner.plan_mode.clone(),
            plan_dir: self.inner.plan_dir.clone(),
            skill_registry: self.inner.skill_registry.clone(),
            active_skills: self.inner.active_skills.clone(),
            shell_filter: self.inner.shell_filter.clone(),
        }
    }

    // ── Commands ────────────────────────────────────────────────

    /// Send a command to the driver task (infallible — unbounded channel).
    pub fn send(&self, cmd: RuntimeCommand) {
        if self.inner.cmd_tx.send(cmd).is_err() {
            tracing::error!("Failed to send RuntimeCommand to driver");
        }
    }

    /// Submit a user message and run the agent loop.
    pub fn run_agent(&self, input: String) {
        self.send(RuntimeCommand::RunAgent(input));
    }

    /// Cancel the currently-running generation.
    pub fn cancel_generation(&self) {
        self.send(RuntimeCommand::CancelGeneration);
    }

    /// Reset the conversation, preserving system prompts.
    pub fn clear_conversation(&self) {
        self.send(RuntimeCommand::ClearConversation);
    }

    /// Execute a user `!command` and share the output with the agent.
    pub fn run_shell(&self, command: String) {
        self.send(RuntimeCommand::RunShell(command));
    }

    /// Route an intervention answer (sandbox approval, ask_user_question,
    /// plan approval) to the waiting requester.
    pub fn respond_intervention(&self, request_id: String, response: InterventionResponse) {
        self.send(RuntimeCommand::InterventionResponse {
            request_id,
            response,
        });
    }

    /// Signal the driver to stop and persist the conversation.
    pub fn shutdown(&self) {
        self.send(RuntimeCommand::Shutdown);
    }

    // ── Sync façade — called directly from the UI thread ─────────

    /// Queue a hint for the next LLM call. Used while the agent is running —
    /// hints must not be pushed straight into memory mid-tool-call (that
    /// would violate the provider API contract).
    pub fn inject_hint(&self, text: String) {
        let mut pending = self
            .inner
            .pending_hints
            .lock()
            .expect("pending hints lock poisoned");
        pending.push(Message::new(Role::User, text));
    }

    /// Save the conversation as a named thread and make it the current one.
    pub fn save_thread(&self, name: &str) -> Result<(), String> {
        let mem = self.inner.memory.read().expect("memory lock poisoned");
        agent_oxide::persistence::save_conversation(
            name,
            &self.inner.workspace_root,
            &mem,
            &self.inner.persistence_config,
        )
        .map_err(|e| format!("Failed to save: {e}"))?;
        let _ = agent_oxide::persistence::write_current_thread_name(
            name,
            &self.inner.workspace_root,
            &self.inner.persistence_config,
        );
        Ok(())
    }

    /// Replace the conversation with a saved thread and make it current.
    pub fn resume_thread(&self, name: &str) -> Result<(), String> {
        let loaded = agent_oxide::persistence::load_conversation(
            name,
            &self.inner.workspace_root,
            &self.inner.persistence_config,
        )
        .map_err(|e| format!("Failed to resume \"{name}\": {e}"))?;
        *self.inner.memory.write().expect("memory lock poisoned") = loaded;
        let _ = agent_oxide::persistence::write_current_thread_name(
            name,
            &self.inner.workspace_root,
            &self.inner.persistence_config,
        );
        Ok(())
    }

    /// List saved threads.
    pub fn list_threads(&self) -> Result<Vec<ThreadInfo>, String> {
        agent_oxide::persistence::list_threads(
            &self.inner.workspace_root,
            &self.inner.persistence_config,
        )
        .map_err(|e| format!("Error listing threads: {e}"))
    }

    /// Activate a skill: add it to the active set for SkillHook to maintain
    /// and inject its instructions into memory immediately. Returns the
    /// skill description on success.
    pub fn load_skill(&self, name: &str) -> Result<String, String> {
        let skill = self
            .inner
            .skill_registry
            .by_name(name)
            .ok_or_else(|| format!("Unknown skill \"{name}\""))?;
        // Add to active skills for the hook to maintain.
        if let Ok(mut active) = self.inner.active_skills.write() {
            active.insert(skill.name.clone(), skill.content.clone());
        }
        // Inject directly into memory for immediate effect. Use
        // insert_before_history so the message lands at the tail of the
        // System block (SkillHook will clean it up and re-insert on the
        // next on_llm_start).
        let msg = format!("[SKILL: {}]\n\n{}", skill.name, skill.content);
        {
            let mut mem = self.inner.memory.write().expect("memory lock poisoned");
            insert_before_history(&mut mem.messages, Message::new(Role::System, msg));
        }
        Ok(skill.description.clone())
    }

    /// Toggle plan mode — the shared atomic backing PlanModeHook.
    pub fn set_plan_mode(&self, active: bool) {
        self.inner.plan_mode.active.store(active, Ordering::SeqCst);
    }

    pub fn plan_mode_active(&self) -> bool {
        self.inner.plan_mode.active.load(Ordering::SeqCst)
    }

    /// Approve the plan (if plan mode is active): archive `.loomis/plan.md`
    /// and deactivate plan mode.
    pub fn approve_plan(&self) -> ApproveOutcome {
        if !self.plan_mode_active() {
            return ApproveOutcome::NotInPlanMode;
        }
        // Archive the plan before deactivating.
        let plan_path = self.inner.workspace_root.join(".loomis").join("plan.md");
        let plan_dir = self.inner.plan_dir.clone();
        let archive = match std::fs::read_to_string(&plan_path) {
            Ok(content) if !content.trim().is_empty() => archive_plan(&content, &plan_dir)
                .map(Some)
                .map_err(|e| format!("Warning: failed to archive plan: {e}")),
            _ => Ok(None),
        };
        self.inner.plan_mode.active.store(false, Ordering::SeqCst);
        ApproveOutcome::Approved { archive }
    }

    /// Memory statistics — `(message count, total chars)`.
    pub fn memory_stats(&self) -> (usize, usize) {
        let mem = self.inner.memory.read().expect("memory lock poisoned");
        (mem.len(), mem.total_chars())
    }

    /// Classify a user `!command` for the frontend's approval modal.
    pub fn classify_shell(&self, command: &str) -> CommandVerdict {
        self.inner.shell_filter.classify(command)
    }

    /// The `/init` prompt — project-rules initialisation instructions,
    /// with an optional trailing user instruction appended.
    pub fn init_prompt(&self, extra: Option<&str>) -> String {
        app::init_prompt(extra)
    }
}

// ── Driver task ────────────────────────────────────────────────────────────

/// Background task that processes [`RuntimeCommand`]s and manages the agent
/// lifecycle.
///
/// When a [`RuntimeCommand::RunAgent`] arrives, the user message is pushed
/// to memory and a new tokio task calls `Agent::run_with_events()`. Events
/// flow back to the frontend through `agent_tx`.
///
/// Cancellation is handled via `JoinHandle::abort()`. Since the agent's own
/// run loop periodically `.await`s (network I/O), abort takes effect
/// quickly.
async fn driver(inner: Arc<RuntimeInner>, mut cmd_rx: mpsc::UnboundedReceiver<RuntimeCommand>) {
    let mut current_run: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            RuntimeCommand::RunAgent(input) => {
                tracing::debug!(
                    input_len = input.chars().count(),
                    "RunAgent command received; spawning agent task",
                );

                // If a previous run is still active, cancel it.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Spawn the agent in a background task.
                // (`run_with_events` pushes the user message to memory internally)
                // Auto-save is handled by PersistenceHook::on_run_finish.
                let tx = inner.agent_tx.clone();
                let agent = Arc::clone(&inner.agent);

                let handle = tokio::spawn(async move {
                    let result =
                        std::panic::AssertUnwindSafe(agent.run_with_events(&input, tx.clone()))
                            .catch_unwind()
                            .await;
                    match result {
                        // Normal completion — PersistenceHook already saved the
                        // conversation in on_run_finish, and the agent loop
                        // already emitted RunCompleted/RunFailed + Done events.
                        Ok(Ok(_)) => {
                            tracing::debug!("Agent task finished normally");
                        }
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "Agent run failed");
                        }
                        // Panic inside the agent loop: tokio would otherwise
                        // swallow it silently. Log it and tell the frontend so
                        // the user isn't left in a stuck "streaming" state.
                        Err(payload) => {
                            let msg = agent_oxide::engine::panic_message(payload.as_ref());
                            tracing::error!(panic = %msg, "Agent task panicked");
                            let _ = tx.send(AgentEvent::RunFailed {
                                error: format!("Agent task panicked: {msg}"),
                            });
                            let _ = tx.send(AgentEvent::Done);
                        }
                    }
                });

                current_run = Some(handle);
            }

            RuntimeCommand::CancelGeneration => {
                tracing::debug!("CancelGeneration command received");
                if let Some(h) = current_run.take() {
                    h.abort();
                    // The agent task is killed immediately — no hooks can run.
                    // Emit cancellation events so the frontend shows proper
                    // feedback.
                    let _ = inner.agent_tx.send(AgentEvent::Cancelled);
                    let _ = inner.agent_tx.send(AgentEvent::Done);
                }
            }

            RuntimeCommand::ClearConversation => {
                tracing::debug!("ClearConversation command received");
                // Cancel any active generation.
                if let Some(h) = current_run.take() {
                    h.abort();
                }

                // Drain memory — preserve only the core system prompts
                // (identified by the [SYSPROMPT] marker).  Everything else
                // is regenerated on the next run: injector hooks
                // (SkillHook, ProfileHook, TodoListHook, PlanModeHook)
                // rebuild their marker messages from canonical state on
                // the first `on_llm_start`, and compaction summaries are
                // stale once the conversation is cleared.
                let mut mem = inner.memory.write().expect("memory lock poisoned");
                let system_msgs: Vec<Message> = mem
                    .to_context_vec()
                    .into_iter()
                    .filter(|m| m.role == Role::System && m.content.starts_with(SYSPROMPT_MARKER))
                    .collect();
                let preserved = system_msgs.len();
                *mem = Memory::new();
                for msg in system_msgs {
                    mem.push(msg);
                }
                drop(mem); // release write lock before read-lock for save

                // Reset the current-thread marker so the next run starts a
                // fresh titled conversation (today `/new` is the only sender).
                let _ = agent_oxide::persistence::write_current_thread_name(
                    &inner.persistence_config.default_thread_name,
                    &inner.workspace_root,
                    &inner.persistence_config,
                );

                // Persist the cleared state.
                {
                    let mem = inner.memory.read().expect("memory lock poisoned");
                    let name = agent_oxide::persistence::default_thread_name(
                        &inner.workspace_root,
                        &inner.persistence_config,
                    );
                    match agent_oxide::persistence::save_conversation(
                        &name,
                        &inner.workspace_root,
                        &mem,
                        &inner.persistence_config,
                    ) {
                        Ok(()) => {
                            tracing::debug!(preserved = preserved, "Cleared conversation persisted",)
                        }
                        Err(e) => tracing::error!(
                            name = %name,
                            error = %e,
                            "Failed to persist cleared conversation",
                        ),
                    }
                }
            }

            RuntimeCommand::InterventionResponse {
                request_id,
                response,
            } => {
                tracing::debug!(
                    request_id = %request_id.chars().take(12).collect::<String>(),
                    "InterventionResponse command received",
                );
                // Route the response to the correct requester
                // (SandboxHook, AskUserQuestionTool, …) via the
                // shared router.  The router removes the sender
                // from its map and delivers the response.
                inner.response_router.route(&request_id, response);
            }

            RuntimeCommand::RunShell(command) => {
                tracing::debug!(
                    cmd = %command.chars().take(200).collect::<String>(),
                    "RunShell command received",
                );
                // Execute the shell command asynchronously — do NOT block
                // the driver or the frontend thread. The command runs
                // in a blocking thread; when it completes, output is
                // pushed to memory and sent to the frontend for display.
                //
                // Use unified ToolCall / ToolSuccessful events with User origin
                // instead of the old ShellRunning / ShellOutput events.
                let shell_id = format!(
                    "shell-{:x}",
                    SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );

                // Notify the frontend that the command is starting.
                let _ = inner.agent_tx.send(AgentEvent::ToolCall {
                    id: shell_id.clone(),
                    name: "shell".into(),
                    arguments: command.clone(),
                    origin: CallOrigin::User,
                });

                let tx = inner.agent_tx.clone();
                let mem = inner.memory.clone();
                let runner = inner.shell_runner.clone();
                let cmd_for_blocking = command.clone();
                let sid = shell_id.clone();

                tokio::spawn(async move {
                    let output = tokio::task::spawn_blocking(move || {
                        execute_shell_command(&runner, &cmd_for_blocking)
                    })
                    .await
                    .unwrap_or_else(|e| format!("Task panicked: {e}"));

                    // Push into shared memory so the LLM sees it
                    {
                        let mut mem = mem.write().expect("memory lock poisoned");
                        mem.push(Message::new(
                            Role::User,
                            format!(
                                "User ran shell command: `{}`\n\nOutput:\n{}",
                                command, output
                            ),
                        ));
                    }

                    // Send result to frontend for display
                    let _ = tx.send(AgentEvent::ToolSuccessful {
                        id: sid,
                        name: "shell".into(),
                        output,
                    });
                });
            }

            RuntimeCommand::Shutdown => {
                tracing::debug!("Shutdown command received; saving conversation");
                // Save conversation before exiting.
                {
                    let mem = inner.memory.read().expect("memory lock poisoned");
                    let name = agent_oxide::persistence::default_thread_name(
                        &inner.workspace_root,
                        &inner.persistence_config,
                    );
                    if let Err(e) = agent_oxide::persistence::save_conversation(
                        &name,
                        &inner.workspace_root,
                        &mem,
                        &inner.persistence_config,
                    ) {
                        tracing::error!(
                            name = %name,
                            error = %e,
                            "Failed to save conversation on exit",
                        );
                    }
                }

                if let Some(h) = current_run.take() {
                    h.abort();
                }
                break;
            }
        }
    }
}
