//! Shell command execution for `!command` (user-initiated) invocations.
//!
//! A thin wrapper over the shared spawn/collect core in
//! [`crate::shell_util`]: spawns a child process in the workspace root,
//! captures stdout/stderr, enforces a fixed 30-second timeout, and decodes
//! output respecting the system ANSI code page on Windows.

use std::path::Path;
use std::time::Duration;

use crate::shell_util::{format_output, run_shell_command};

/// Executes a shell command in the workspace root, capturing stdout and stderr.
///
/// On Windows, uses `cmd /C` for near-instant startup (unlike PowerShell which
/// loads .NET CLR on every invocation). Encoding and truncation are handled by
/// the shared core in [`crate::shell_util`].
///
/// Environment is sanitised before spawning (same as the LLM-facing
/// ShellTool), and output is truncated at 100 KB to prevent flooding
/// the conversation context.
pub fn execute_shell_command(command: &str, workspace_root: &Path) -> String {
    match run_shell_command(command, workspace_root, Duration::from_secs(30), true) {
        Ok(result) => format_output(&result, "\n"), // flat stderr — no [stderr] marker
        Err(e) => e,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_oxide::sandbox::encoding::MAX_OUTPUT_BYTES;
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    #[test]
    fn test_execute_echo() {
        #[cfg(target_os = "windows")]
        let cmd = "echo hello";
        #[cfg(not(target_os = "windows"))]
        let cmd = "echo hello";

        let output = execute_shell_command(cmd, &workspace_root());
        assert!(output.contains("hello"), "got: {output}");
    }

    #[test]
    fn test_execute_empty_output() {
        #[cfg(target_os = "windows")]
        let cmd = "cd .";
        #[cfg(not(target_os = "windows"))]
        let cmd = "true";

        let output = execute_shell_command(cmd, &workspace_root());
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

        let output = execute_shell_command(cmd, &workspace_root());
        assert!(output.contains("error text"), "got: {output}");
    }

    #[test]
    fn test_execute_non_zero_exit() {
        #[cfg(target_os = "windows")]
        let cmd = "cmd /C exit /b 42";
        #[cfg(not(target_os = "windows"))]
        let cmd = "exit 42";

        let output = execute_shell_command(cmd, &workspace_root());
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

        let output = execute_shell_command(cmd, &workspace_root());
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
        let output = execute_shell_command("nonexistent_command_xyz_123", &workspace_root());
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

        let output = execute_shell_command(cmd, &workspace_root());
        // The output should be truncated
        assert!(
            output.contains("[output truncated at") || output.len() <= MAX_OUTPUT_BYTES + 100,
            "large output should be truncated, got {} bytes",
            output.len()
        );
    }
}
