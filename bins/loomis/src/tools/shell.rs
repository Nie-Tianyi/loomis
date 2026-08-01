//! # ShellTool — Command-line execution tool
//!
//! Executes shell commands in the workspace directory with a configurable
//! timeout. Used by the agent to run CLI tools, build scripts, tests, etc.
//!
//! ## Safety
//!
//! Commands are validated through [`ShellFilter`](sandbox::shell_filter::ShellFilter)
//! before execution.  The environment is sanitised via
//! [`sanitize`](sandbox::env_sanitizer::sanitize) so that secrets
//! and dangerous variables (`LD_PRELOAD`, …) are not leaked to child
//! processes.  A watchdog thread enforces the timeout and kills the
//! **entire process tree** (not just the immediate child) on timeout.
//! Output is capped at 100 KB.
//!
//! ## User confirmation
//!
//! The [`SandboxHook`](crate::hooks::SandboxHook) intercepts shell tool
//! calls before they reach `execute()`.  Commands matching an auto-approve
//! prefix run immediately; commands matching a deny-pattern are blocked;
//! everything else prompts the user.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Deserialize;

use tools::{ProgressStream, ToolError, tool};

use sandbox::SandboxConfig;

use sandbox::encoding::{self, MAX_OUTPUT_BYTES};
use sandbox::env_sanitizer;
use sandbox::shell_filter::ShellFilter;
use sandbox::watchdog::Watchdog;

/// Arguments for shell command execution.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShellArgs {
    /// The shell command to execute.
    #[schemars(
        description = "The shell command to execute. Runs with the workspace root as working directory. On Windows: cmd /C. On Unix: sh -c. Examples: 'cargo build', 'git status', 'npm test'. Do NOT use for cat/ls/find/grep/echo — use the dedicated tools instead."
    )]
    pub command: String,

    /// Max execution time in seconds.
    #[schemars(
        description = "Max execution time in seconds (range: 1-120). Default: 60. The process is killed if exceeded; partial output captured so far is returned. Set shorter for quick commands, longer for builds."
    )]
    pub timeout_secs: Option<u64>,
}

/// Executes arbitrary shell commands within the workspace.
///
/// # Platform shells
///
/// | OS | Shell | Invocation |
/// |----|-------|-----------|
/// | Windows | `cmd.exe` | `cmd /C <command>` |
/// | Unix | `sh` | `sh -c <command>` |
#[tool(
    name = "shell",
    description = "Execute a shell command in the workspace directory. The command runs inside \
         the workspace root as the working directory.\n\n\
         Output is capped at 100 KB to avoid flooding context. If the command \
         exceeds the timeout it is killed and partial output is returned. Exit code \
         is appended to the output when non-zero.\n\n\
         When to use: running build commands (`cargo build`, `npm install`, `make`), \
         running tests (`cargo test`, `pytest`), version control (`git status`, \
         `git diff`, `git log`), any CLI tool without a dedicated equivalent.\n\n\
         IMPORTANT — use dedicated tools instead of shell when possible:\n\
         - Reading files → use read (safer, cat -n format with line numbers)\n\
         - Listing directories → use ls or glob (structured output)\n\
         - Searching content → use grep (structured output with line numbers)\n\
         - Editing files → use edit or write (sandbox-safe, undoable)\n\
         Do NOT use shell to run `cat`, `ls`, `find`, `grep`, `echo`, or `sed` \
         unless you have verified that the dedicated tool cannot accomplish the task.\n\n\
         Timed out or killed commands return partial output — do not assume success \
         when output is incomplete.",
    args = ShellArgs
)]
pub struct ShellTool {
    /// All commands run with this as the working directory.
    workspace_root: PathBuf,
    /// Default timeout applied when the model omits `timeout_secs`.
    default_timeout: Duration,
    /// Hard upper bound — the model cannot request more.
    max_timeout: Duration,
    /// Whether to sanitize the environment before spawning.
    sanitize_env: bool,
    /// Compiled command classifier (auto-approve / deny / prompt).
    filter: ShellFilter,
}

impl ShellTool {
    /// Creates a new shell tool from sandbox configuration.
    pub fn new(workspace_root: PathBuf, config: &SandboxConfig) -> Self {
        Self {
            workspace_root,
            default_timeout: Duration::from_secs(config.shell.default_timeout_secs),
            max_timeout: Duration::from_secs(config.shell.max_timeout_secs),
            sanitize_env: config.shell.sanitize_environment,
            filter: ShellFilter::from_config(config),
        }
    }

    fn execute_stream(&self, args: ShellArgs) -> Result<ProgressStream, ToolError> {
        let command = args.command;
        if command.trim().is_empty() {
            return Err(ToolError::InvalidArgs(
                "Missing required field: 'command'".into(),
            ));
        }

        let command_preview: String = command.chars().take(300).collect();

        // ── Command validation ────────────────────────────────────────
        use sandbox::shell_filter::CommandVerdict;
        if let CommandVerdict::Blocked { reason } = self.filter.classify(&command) {
            tracing::warn!(
                command = %command_preview,
                reason = %reason,
                "Shell command blocked by sandbox policy"
            );
            return Err(ToolError::Execution(format!(
                "Command blocked by sandbox policy: {reason}"
            )));
        }

        let timeout_secs = args
            .timeout_secs
            .unwrap_or(self.default_timeout.as_secs())
            .min(self.max_timeout.as_secs())
            .max(1);

        // ── Platform shell selection ──────────────────────────────────
        // Windows: use `cmd /S /C "<command>"` built with raw_arg. cmd's
        // own quote-stripping rules are incompatible with Rust's CRT-style
        // argument escaping (`\"` inside the command mangles inner quotes).
        // `/S` makes cmd strip only the outermost quote pair, preserving
        // inner quotes — so `git commit -m "msg"` and `findstr /c:"a b"` work.
        #[cfg(target_os = "windows")]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("cmd");
            c.raw_arg("/S");
            c.raw_arg("/C");
            c.raw_arg(format!("\"{command}\""));
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.arg("-c");
            c.arg(&command);
            c
        };

        // ── Spawn child process ───────────────────────────────────────
        cmd.current_dir(&self.workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Apply environment sanitization
        env_sanitizer::sanitize(&mut cmd, &self.workspace_root, self.sanitize_env);

        let start = Instant::now();

        let child = cmd.spawn().map_err(|e| {
            tracing::error!(
                command = %command_preview,
                error = %e,
                "Failed to spawn shell command"
            );
            ToolError::Execution(format!("Failed to spawn command: {e}"))
        })?;

        let pid = child.id();

        // ── Watchdog (kills entire process tree on timeout) ───────
        let watchdog = Watchdog::spawn(pid, Duration::from_secs(timeout_secs));

        // ── Wait for process ─────────────────────────────────────
        let output = child
            .wait_with_output()
            .map_err(|e| ToolError::Execution(format!("Failed to wait on command: {e}")))?;

        // Signal the watchdog to exit, then join (returns within 100ms).
        watchdog.disarm();

        // ── Build result ─────────────────────────────────────────
        let stdout = encoding::decode_stdout(&output.stdout);
        let stderr = encoding::decode_stdout(&output.stderr);
        let exit_code = output.status.code();

        let mut result = String::new();

        let stdout_clean = stdout.trim_end();
        let stderr_clean = stderr.trim_end();

        if !stdout_clean.is_empty() {
            result.push_str(&encoding::truncate_output(stdout_clean, MAX_OUTPUT_BYTES));
        }

        if !stderr_clean.is_empty() {
            if !result.is_empty() {
                result.push_str("\n\n[stderr]\n");
            }
            // Reserve ~20% of budget for stderr (or at least 10KB)
            let stderr_max = (MAX_OUTPUT_BYTES / 5).max(10_240);
            // But don't exceed remaining budget
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(result.len());
            let stderr_limit = stderr_max.min(remaining);
            result.push_str(&encoding::truncate_output(stderr_clean, stderr_limit));
        }

        // If nothing was produced, still indicate the command ran
        if result.is_empty() {
            match exit_code {
                Some(0) => result.push_str("(command completed with no output)"),
                Some(code) => {
                    result.push_str(&format!("(exit code: {code}, no output)"));
                }
                None => result.push_str("(process terminated by signal, no output)"),
            }
        } else if let Some(code) = exit_code
            && code != 0
        {
            // Append exit code info after output
            result.push_str(&format!("\n\n[exit code: {code}]"));
        }

        let elapsed_ms = start.elapsed().as_millis();
        match exit_code {
            Some(0) if stderr_clean.is_empty() => {
                tracing::info!(
                    command = %command_preview,
                    exit_code = 0,
                    elapsed_ms,
                    "Shell command completed"
                );
            }
            Some(0) => {
                tracing::warn!(
                    command = %command_preview,
                    exit_code = 0,
                    stderr_len = stderr_clean.len(),
                    elapsed_ms,
                    "Shell command completed with stderr output"
                );
            }
            Some(code) => {
                tracing::error!(
                    command = %command_preview,
                    exit_code = code,
                    elapsed_ms,
                    "Shell command failed"
                );
            }
            None => {
                tracing::error!(
                    command = %command_preview,
                    elapsed_ms,
                    "Shell command terminated by signal (likely watchdog timeout)"
                );
            }
        }

        let output = result;
        Ok(ProgressStream::done(output))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn make_tool() -> ShellTool {
        ShellTool::new(std::env::current_dir().unwrap(), &SandboxConfig::default())
    }

    // ── Metadata ──────────────────────────────────────────────────

    #[test]
    fn test_name() {
        let tool = make_tool();
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn test_description() {
        let tool = make_tool();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_parameters_schema() {
        let tool = make_tool();
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["command"]["type"] == "string");
        assert!(params["required"][0] == "command");
        assert_eq!(
            params["additionalProperties"], false,
            "ShellTool must include additionalProperties: false"
        );
    }

    // ── Execution ─────────────────────────────────────────────────

    #[test]
    fn test_execute_echo() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, r#"{"command": "echo hello"}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("hello"), "got: {result}");
    }

    /// Regression: quoted arguments must survive to the shell.
    /// `findstr /c:` with a space in the search string used to be mangled
    /// by CRT-style escaping on Windows (cmd /S /C handling).
    #[cfg(target_os = "windows")]
    #[test]
    fn test_execute_quoted_findstr() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("probe.txt"), "alpha beta\n").unwrap();
        let tool = ShellTool::new(dir.path().to_path_buf(), &SandboxConfig::default());
        let result = Tool::execute_stream(
            &tool,
            r#"{"command": "findstr /n /c:\"alpha beta\" probe.txt"}"#,
        )
        .unwrap()
        .poll_done();
        assert!(result.contains("alpha beta"), "got: {result}");
    }

    #[test]
    fn test_execute_quoted_echo() {
        let tool = make_tool();
        // Quotes must be preserved, not escaped into backslash-quotes.
        #[cfg(target_os = "windows")]
        let expected = "\"hello world\"";
        #[cfg(not(target_os = "windows"))]
        let expected = "hello world";
        let result = Tool::execute_stream(&tool, r#"{"command": "echo \"hello world\""}"#)
            .unwrap()
            .poll_done();
        let trimmed = result.trim();
        assert_eq!(trimmed, expected, "got: {result}");
        assert!(
            !result.contains("\\\""),
            "backslash-quote escaping must not leak to cmd: {result}"
        );
    }

    #[test]
    fn test_execute_pwd() {
        let tool = make_tool();
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "echo %cd%"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "pwd"}"#;
        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_execute_non_zero_exit() {
        let tool = make_tool();
        // exit /b 42 works on Windows; exit 42 works on Unix
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cmd /C exit /b 42"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "exit 42"}"#;

        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        // Should mention the exit code
        assert!(
            result.contains("exit code") || result.contains("42"),
            "got: {result}"
        );
    }

    #[test]
    fn test_execute_missing_command() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, r#"{"timeout_secs": 5}"#);
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("command"), "got: {msg}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_empty_command() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, r#"{"command": "   "}"#);
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("command"), "got: {msg}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_bad_json() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, "not json");
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("invalid args"), "got: {msg}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_no_output() {
        let tool = make_tool();
        // A command that produces no output at all
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cd ."}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "true"}"#;

        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        // Should indicate the command ran even though there's no output
        assert!(
            result.contains("no output") || result.is_empty(),
            "got: {result}"
        );
    }

    #[test]
    fn test_execute_with_timeout_in_args() {
        let tool = make_tool();
        let mut result =
            Tool::execute_stream(&tool, r#"{"command": "echo fast", "timeout_secs": 10}"#)
                .expect("should succeed");
        let output = result.poll_done();
        assert!(output.contains("fast"), "got: {output}");
    }

    #[test]
    fn test_execute_stderr_captured() {
        let tool = make_tool();
        // Print to stderr
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cmd /C echo error text >&2"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "echo error text >&2"}"#;

        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        assert!(result.contains("error text"), "got: {result}");
    }
}
