//! Environment sanitizer for sandboxed child processes.
//!
//! When `sanitize_environment` is enabled, we clear all environment
//! variables and only pass a known-safe allowlist.  This prevents
//! leaking secrets (`DEEPSEEK_API`, etc.) and neutralises
//! injection vectors like `LD_PRELOAD`.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Apply environment sanitization to a [`Command`] before spawning.
///
/// When `enabled` is true:
/// - All variables are cleared.
/// - Only the safe allowlist below is restored.
/// - `LD_PRELOAD` and friends are explicitly removed.
/// - `workspace_root/bin` is prepended to `PATH`.
pub fn sanitize(cmd: &mut Command, workspace_root: &Path, enabled: bool) {
    if !enabled {
        return;
    }

    // Save values before clearing.
    let preserved = collect_safe_vars();

    cmd.env_clear();

    // Restore safe variables
    for (key, val) in &preserved {
        cmd.env(key, val);
    }

    // Prepend workspace bin to PATH so project-local tools are available.
    let ws_bin = workspace_root.join("bin");
    if ws_bin.is_dir() {
        let separator = if cfg!(target_os = "windows") {
            ";"
        } else {
            ":"
        };
        if let Some(existing_path) = preserved.get("PATH") {
            cmd.env(
                "PATH",
                format!("{}{}{}", ws_bin.display(), separator, existing_path),
            );
        } else {
            cmd.env("PATH", ws_bin.display().to_string());
        }
    }
}

/// Returns the values of environment variables that are safe to pass
/// to child processes.
fn collect_safe_vars() -> std::collections::HashMap<String, String> {
    // Variables we consider safe for child processes.
    let safe_keys: HashSet<&str> = [
        // Standard
        "PATH",
        "HOME",
        "USER",
        "USERNAME",
        "TEMP",
        "TMP",
        "TMPDIR",
        "SHELL",
        "LANG",
        "LC_ALL",
        // Windows
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMDATA",
        "APPDATA",
        "LOCALAPPDATA",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        // Dev tooling
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTC_WRAPPER",
        "NPM_CONFIG_USERCONFIG",
        "NODE_PATH",
        "PYTHONPATH",
        "GOPATH",
        "JAVA_HOME",
        // Terminal / display
        "TERM",
        "COLORTERM",
        "NO_COLOR",
        "CLICOLOR",
        "FORCE_COLOR",
        // CI
        "CI",
        "GITHUB_ACTIONS",
        // pkg-config
        "PKG_CONFIG_PATH",
    ]
    .into_iter()
    .collect();

    let mut preserved = std::collections::HashMap::new();

    for key in &safe_keys {
        if let Ok(val) = std::env::var(key) {
            preserved.insert(key.to_string(), val);
        }
    }

    preserved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_does_not_crash() {
        // Smoke test — verify sanitize() doesn't panic with
        // various inputs.
        let mut cmd = std::process::Command::new("echo");
        sanitize(&mut cmd, Path::new("/tmp"), true);
        // After sanitize, the command should still be spawnable
        // (we just can't inspect its env from here).
        drop(cmd);
    }
}
