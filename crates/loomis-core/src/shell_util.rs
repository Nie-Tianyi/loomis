//! Shared shell spawn/collect core for [`tools::shell::ShellTool`]
//! (LLM-facing) and [`execute_shell_command`] (user `!command`).
//!
//! The two callers keep their own output formatting (ShellTool prefixes
//! non-failed stderr with `[stderr]`, `!command` flattens it), but the
//! platform shell selection, env sanitisation, watchdog, decode, and
//! truncation logic is identical — and must stay identical so both paths
//! behave the same.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use agent_oxide::sandbox::encoding::{self, MAX_OUTPUT_BYTES};
use agent_oxide::sandbox::env_sanitizer;
use agent_oxide::sandbox::watchdog::Watchdog;

/// Structured outcome of a completed command.
pub struct ShellResult {
    /// Decoded stdout (not yet trimmed).
    pub stdout: String,
    /// Decoded stderr.
    pub stderr: String,
    /// Process exit code — `None` when terminated by a signal (watchdog
    /// timeout).
    pub exit_code: Option<i32>,
}

/// Spawn `command` in `workspace_root`, wait for it under a watchdog, and
/// return its decoded output. Errors (spawn/wait failure) return a
/// human-readable message — both callers surface it verbatim.
pub fn run_shell_command(
    command: &str,
    workspace_root: &Path,
    timeout: Duration,
    sanitize_env: bool,
) -> Result<ShellResult, String> {
    // Windows: `cmd /S /C "<command>"` via raw_arg so inner quotes survive.
    // cmd's own quote-stripping is incompatible with Rust's CRT-style
    // argument escaping (`\"` inside the command mangles inner quotes).
    // `/S` makes cmd strip only the outermost quote pair, preserving inner
    // quotes — so `git commit -m "msg"` and `findstr /c:"a b"` work.
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

    // Sanitize environment before spawning — secrets and dangerous
    // variables (`LD_PRELOAD`, …) must not leak to child processes.
    env_sanitizer::sanitize(&mut cmd, workspace_root, sanitize_env);

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn command: {e}"))?;
    let pid = child.id();

    // Watchdog: polls every 100ms, kills the entire process tree if the
    // timeout is exceeded. The AtomicBool signal makes disarm() return
    // within 100ms even for fast commands.
    let watchdog = Watchdog::spawn(pid, timeout);
    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait on command: {e}"))?;
    watchdog.disarm();

    Ok(ShellResult {
        stdout: encoding::decode_stdout(&output.stdout),
        stderr: encoding::decode_stdout(&output.stderr),
        exit_code: output.status.code(),
    })
}

/// Combine decoded stdout/stderr into the canonical capped output.
///
/// `stderr_separator` distinguishes the two display formats: ShellTool uses
/// `"\n\n[stderr]\n"` (section marker), `!command` uses `"\n"` (flat).
pub fn format_output(result: &ShellResult, stderr_separator: &str) -> String {
    let stdout_clean = result.stdout.trim_end();
    let stderr_clean = result.stderr.trim_end();
    let exit_code = result.exit_code;

    let mut combined = String::new();
    if !stdout_clean.is_empty() {
        combined.push_str(&encoding::truncate_output(stdout_clean, MAX_OUTPUT_BYTES));
    }

    // Reserve ~20% of budget for stderr (or at least 10KB).
    let stderr_max = (MAX_OUTPUT_BYTES / 5).max(10_240);

    // Failed commands get a prominent error block: exit code first, then
    // stderr — so the failure reason is scannable at a glance, even when
    // the error went to stdout or nowhere at all.
    if let Some(code) = exit_code.filter(|&c| c != 0) {
        if !combined.is_empty() {
            combined.push_str("\n\n");
        }
        combined.push_str(&format!("[FAILED — exit code: {code}]"));
        if !stderr_clean.is_empty() {
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(combined.len());
            let stderr_limit = stderr_max.min(remaining);
            combined.push('\n');
            combined.push_str(&encoding::truncate_output(stderr_clean, stderr_limit));
        }
    } else if !stderr_clean.is_empty() {
        if !combined.is_empty() {
            combined.push_str(stderr_separator);
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(combined.len());
        let stderr_limit = stderr_max.min(remaining);
        combined.push_str(&encoding::truncate_output(stderr_clean, stderr_limit));
    }

    // If nothing was produced, still indicate the command ran.
    // (A non-zero exit code always produces the [FAILED — …] block above.)
    if combined.is_empty() {
        match exit_code {
            Some(0) => combined.push_str("(command completed with no output)"),
            None => combined.push_str("(process terminated by signal, no output)"),
            Some(_) => {}
        }
    }

    combined
}
