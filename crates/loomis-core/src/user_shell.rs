//! Shell command execution for `!command` (user-initiated) invocations.
//!
//! Delegates to the library's [`ShellRunner`](agent_oxide::sandbox::ShellRunner)
//! (the same execution chain the LLM-facing ShellTool uses): spawn in the
//! workspace root, sanitised environment, tree-kill watchdog, bounded
//! output capture.  A fixed 30-second timeout applies — the policy check
//! (`ShellFilter::classify`) happens before the command is sent here, via
//! [`Runtime::classify_shell`](crate::runtime::Runtime::classify_shell).

use agent_oxide::sandbox::shell_runner::ShellRunner;

use crate::shell_util::format_output;

/// Executes a shell command in the workspace root, capturing stdout and stderr.
///
/// On Windows, uses `cmd /D /S /C` for near-instant startup (unlike
/// PowerShell which loads .NET CLR on every invocation) with the AutoRun
/// registry hook disabled. Encoding and truncation are handled by the
/// library execution chain; output is formatted and capped at 100 KB to
/// prevent flooding the conversation context.
///
/// The command must already have passed `ShellFilter::classify` — this
/// function is deliberately policy-free, like `ShellRunner::run` itself.
pub fn execute_shell_command(runner: &ShellRunner, command: &str) -> String {
    match runner.run(command, Some(30)) {
        Ok(result) => format_output(&result, "\n"), // flat stderr — no [stderr] marker
        Err(e) => e.to_string(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_oxide::sandbox::SandboxConfig;
    use agent_oxide::sandbox::encoding::MAX_OUTPUT_BYTES;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    fn make_runner(ws: &Path) -> ShellRunner {
        ShellRunner::new(ws.to_path_buf(), SandboxConfig::default().shell.clone())
    }

    #[test]
    fn test_execute_echo() {
        #[cfg(target_os = "windows")]
        let cmd = "echo hello";
        #[cfg(not(target_os = "windows"))]
        let cmd = "echo hello";

        let output = execute_shell_command(&make_runner(&workspace_root()), cmd);
        assert!(output.contains("hello"), "got: {output}");
    }

    #[test]
    fn test_execute_empty_output() {
        #[cfg(target_os = "windows")]
        let cmd = "cd .";
        #[cfg(not(target_os = "windows"))]
        let cmd = "true";

        let output = execute_shell_command(&make_runner(&workspace_root()), cmd);
        assert!(
            output.contains("no output") || output.is_empty(),
            "got: {output}"
        );
    }

    #[test]
    fn test_execute_stderr_captured() {
        #[cfg(target_os = "windows")]
        let cmd = "cmd /C echo error text >&2";
        #[cfg(not(target_os = "windows"))]
        let cmd = "echo error text >&2";

        let output = execute_shell_command(&make_runner(&workspace_root()), cmd);
        assert!(output.contains("error text"), "got: {output}");
    }

    #[test]
    fn test_execute_non_zero_exit() {
        #[cfg(target_os = "windows")]
        let cmd = "cmd /C exit /b 42";
        #[cfg(not(target_os = "windows"))]
        let cmd = "exit 42";

        let output = execute_shell_command(&make_runner(&workspace_root()), cmd);
        assert!(
            output.contains("exit code") || output.contains("42"),
            "got: {output}"
        );
    }

    #[test]
    fn test_failed_command_shows_exit_code_then_stderr() {
        #[cfg(target_os = "windows")]
        let cmd = "cmd /C echo boom >&2 & exit /b 3";
        #[cfg(not(target_os = "windows"))]
        let cmd = "echo boom >&2; exit 3";

        let output = execute_shell_command(&make_runner(&workspace_root()), cmd);
        let marker = output
            .find("[FAILED — exit code: 3]")
            .expect("FAILED marker");
        let stderr_pos = output.find("boom").expect("stderr content");
        assert!(
            marker < stderr_pos,
            "exit code marker must precede stderr: {output}"
        );
    }

    #[test]
    fn test_execute_missing_command() {
        let output = execute_shell_command(
            &make_runner(&workspace_root()),
            "nonexistent_command_xyz_123",
        );
        // Should not panic; output may contain an error or exit code.
        assert!(!output.is_empty(), "should produce some output");
    }

    #[test]
    fn test_large_output_truncated() {
        // Generate a command that outputs >100KB of text
        #[cfg(target_os = "windows")]
        let cmd = "powershell -Command \"'x' * 200000\"";
        #[cfg(not(target_os = "windows"))]
        let cmd = "printf 'x%.0s' $(seq 1 200000)";

        let output = execute_shell_command(&make_runner(&workspace_root()), cmd);
        // The output should be truncated (bounded capture marks it too).
        assert!(
            output.contains("truncated") || output.len() <= MAX_OUTPUT_BYTES + 100,
            "large output should be truncated, got {} bytes",
            output.len()
        );
    }
}
