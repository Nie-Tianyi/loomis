//! Shared output formatting for the two shell paths:
//! [`tools::shell::ShellTool`](crate::tools::shell::ShellTool) (LLM-facing)
//! and [`execute_shell_command`](crate::user_shell::execute_shell_command)
//! (user `!command`).
//!
//! Both callers execute through the library's
//! [`ShellRunner`](agent_oxide::sandbox::ShellRunner) (env sanitisation,
//! tree watchdog, bounded capture, decoding — the full execution chain),
//! but keep their own output formatting (ShellTool prefixes non-failed
//! stderr with `[stderr]`, `!command` flattens it).  That formatting is
//! identical by construction — and must stay identical so both paths
//! behave the same.

use agent_oxide::sandbox::encoding::{self, MAX_OUTPUT_BYTES};
use agent_oxide::sandbox::shell_runner::ShellOutput;

/// Combine decoded stdout/stderr into the canonical capped output.
///
/// `stderr_separator` distinguishes the two display formats: ShellTool uses
/// `"\n\n[stderr]\n"` (section marker), `!command` uses `"\n"` (flat).
pub fn format_output(result: &ShellOutput, stderr_separator: &str) -> String {
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
        match (exit_code, result.timed_out) {
            (Some(0), _) => combined.push_str("(command completed with no output)"),
            (None, true) => {
                combined.push_str("(command timed out — killed by watchdog, no output)")
            }
            (None, false) => combined.push_str("(process terminated by signal, no output)"),
            (Some(_), _) => {}
        }
    }

    // Trailing markers — surfaced when the run produced output but was cut
    // short (watchdog kill or output budget exceeded).  `truncated` implies
    // the output budget was hit and the process tree killed at read time.
    if result.timed_out {
        combined.push_str("\n[timed out — killed by watchdog]");
    }
    if result.truncated {
        combined.push_str("\n[output truncated]");
    }

    combined
}
