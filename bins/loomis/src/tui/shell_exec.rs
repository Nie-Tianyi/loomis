//! Shell command execution for `!command` (user-initiated) invocations.
//!
//! Spawns a child process in the workspace root, captures stdout/stderr,
//! enforces a 30-second timeout via [`Watchdog`](agent_oxide::sandbox::watchdog::Watchdog),
//! and decodes output respecting the system ANSI code page on Windows via
//! [`decode_stdout`](agent_oxide::sandbox::encoding::decode_stdout).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use agent_oxide::sandbox::encoding::{self, MAX_OUTPUT_BYTES};
use agent_oxide::sandbox::env_sanitizer;
use agent_oxide::sandbox::watchdog::Watchdog;

/// Executes a shell command in the workspace root, capturing stdout and stderr.
///
/// On Windows, uses `cmd /C` for near-instant startup (unlike PowerShell which
/// loads .NET CLR on every invocation). Encoding is handled via
/// [`encoding::decode_stdout`], which tries UTF-8 first and falls back to the
/// system ANSI code page.
///
/// Environment is sanitised via [`env_sanitizer::sanitize`] before spawning,
/// and output is truncated at [`MAX_OUTPUT_BYTES`] to prevent flooding
/// the conversation context.
pub fn execute_shell_command(command: &str, workspace_root: &Path) -> String {
    // Windows: `cmd /S /C "<command>"` via raw_arg so inner quotes survive.
    // See ShellTool for the full rationale — cmd's quote-stripping is
    // incompatible with Rust's CRT-style argument escaping.
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
        c.arg(command);
        c
    };

    cmd.current_dir(workspace_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Sanitize environment before spawning (same as LLM-facing ShellTool).
    env_sanitizer::sanitize(&mut cmd, workspace_root, true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("Failed to spawn command: {e}"),
    };

    let pid = child.id();

    // Watchdog: polls every 100ms, kills the process if it exceeds the
    // timeout. The Watchdog struct encapsulates the AtomicBool signal
    // so disarm() returns within 100ms even for fast commands.
    let watchdog = Watchdog::spawn(pid, Duration::from_secs(30));

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return format!("Failed to wait on command: {e}"),
    };

    // Signal the watchdog that the command is done, then join.
    watchdog.disarm();

    let stdout = encoding::decode_stdout(&output.stdout);
    let stderr = encoding::decode_stdout(&output.stderr);
    let exit_code = output.status.code();

    let stdout_clean = stdout.trim_end();
    let stderr_clean = stderr.trim_end();

    let mut result = String::new();
    if !stdout_clean.is_empty() {
        result.push_str(&encoding::truncate_output(stdout_clean, MAX_OUTPUT_BYTES));
    }

    // Reserve ~20% of budget for stderr (or at least 10KB).
    let stderr_max = (MAX_OUTPUT_BYTES / 5).max(10_240);

    // Failed commands get a prominent error block: exit code first, then
    // stderr — same format as ShellTool, so agent and `!command` output match.
    if let Some(code) = exit_code.filter(|&c| c != 0) {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&format!("[FAILED — exit code: {code}]"));
        if !stderr_clean.is_empty() {
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(result.len());
            let stderr_limit = stderr_max.min(remaining);
            result.push('\n');
            result.push_str(&encoding::truncate_output(stderr_clean, stderr_limit));
        }
    } else if !stderr_clean.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(result.len());
        let stderr_limit = stderr_max.min(remaining);
        result.push_str(&encoding::truncate_output(stderr_clean, stderr_limit));
    }

    // If nothing was produced, indicate the command ran.
    // (A non-zero exit code always produces the [FAILED — …] block above.)
    if result.is_empty() {
        match exit_code {
            Some(0) => result.push_str("(command completed with no output)"),
            None => result.push_str("(process terminated by signal, no output)"),
            Some(_) => {}
        }
    }

    result
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
