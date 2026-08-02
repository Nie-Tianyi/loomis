//! Platform-aware keyboard shortcut helpers.
//!
//! Clipboard shortcuts should follow the OS convention the user expects:
//!
//! | Platform      | Copy     | Paste     | Cancel     |
//! |---------------|----------|-----------|------------|
//! | macOS         | Cmd+C    | Cmd+V     | Ctrl+C     |
//! | Windows/Linux | Ctrl+C   | Ctrl+V    | Ctrl+C     |
//!
//! The distinction matters most on macOS, where `Ctrl+C` is the universal
//! terminal *interrupt* key — it cancels the agent, it never copies. Copying
//! selected text is `Cmd+C`, reported by crossterm as
//! [`KeyModifiers::SUPER`].
//!
//! All checks are `cfg!(target_os = "macos")` compile-time constants — zero
//! runtime cost, consistent with the `#[cfg(target_os)]` pattern used in
//! [`super::shell_exec`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Returns `true` when `key` matches the platform-appropriate **paste**
/// shortcut.
///
/// On macOS this accepts **both** Cmd+V and Ctrl+V: Cmd+V is the native
/// binding, and Ctrl+V stays as a fallback for terminals that capture Cmd+V
/// for their own clipboard handling (most macOS terminals do). On other
/// platforms only Ctrl+V is recognised.
pub fn is_paste_shortcut(key: &KeyEvent) -> bool {
    let has_paste_modifier = if cfg!(target_os = "macos") {
        key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
    } else {
        key.modifiers.contains(KeyModifiers::CONTROL)
    };
    key.code == KeyCode::Char('v') && has_paste_modifier
}

/// Returns `true` when `key` matches the platform-appropriate **copy**
/// shortcut.
///
/// # Platform behaviour
///
/// | Platform   | Copy key          | Ctrl+C behaviour               |
/// |------------|-------------------|--------------------------------|
/// | macOS      | Cmd+C (SUPER+C)   | Always cancel (never copy)     |
/// | Other      | Ctrl+C            | Copy when selection, else cancel |
///
/// This split keeps `Ctrl+C` as the universal "interrupt" key while giving
/// macOS users their native `Cmd+C` copy.
pub fn is_copy_shortcut(key: &KeyEvent) -> bool {
    let has_modifier = key.modifiers.contains(copy_modifier());
    key.code == KeyCode::Char('c') && has_modifier
}

/// Returns `true` when `key` should be treated as a cancel / interrupt —
/// it stops a running generation regardless of text-selection state.
///
/// On macOS this is `Ctrl+C` only (`Cmd+C` is copy-only, see
/// [`is_copy_shortcut`]). On other platforms this is identical to
/// [`is_copy_shortcut`] — one key serves both copy and cancel.
pub fn is_cancel_shortcut(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Returns `true` when `key` carries a shortcut modifier — Ctrl on every
/// platform, plus Cmd on macOS.
///
/// A modified key is a chord, never typed text: an unbound chord must not
/// insert its letter (Ctrl+Z must not type 'z', and on macOS Cmd+Z must
/// not type 'z' either). This is the guard the character-insertion path
/// uses to swallow chords it does not understand.
pub fn has_shortcut_modifier(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        || (cfg!(target_os = "macos") && key.modifiers.contains(KeyModifiers::SUPER))
}

/// The modifier that triggers **copy** on this platform.
///
/// `KeyModifiers::SUPER` on macOS (the Command key), `KeyModifiers::CONTROL`
/// elsewhere.
fn copy_modifier() -> KeyModifiers {
    if cfg!(target_os = "macos") {
        KeyModifiers::SUPER
    } else {
        KeyModifiers::CONTROL
    }
}

/// Human-readable label for the **copy** shortcut, for help text and hints.
///
/// Returns `"Cmd+C"` on macOS and `"Ctrl+C"` on other platforms.
pub fn copy_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+C"
    } else {
        "Ctrl+C"
    }
}

/// Human-readable label for the **paste** shortcut, for help text and hints.
///
/// Returns `"Cmd+V"` on macOS and `"Ctrl+V"` on other platforms.
pub fn paste_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+V"
    } else {
        "Ctrl+V"
    }
}

/// Human-readable label for the **cancel** shortcut, for help text and hints.
///
/// Always `"Ctrl+C"` — cancel is the same key on every platform.
pub fn cancel_shortcut_label() -> &'static str {
    "Ctrl+C"
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a key event for `char` with the given modifiers.
    fn key_with(char: char, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(char), modifiers)
    }

    // ── Paste ────────────────────────────────────────────────────────

    #[test]
    fn test_paste_shortcut_always_accepts_ctrl_v() {
        // Ctrl+V must paste on every platform — the fallback binding.
        let key = key_with('v', KeyModifiers::CONTROL);
        assert!(is_paste_shortcut(&key));
    }

    #[test]
    fn test_paste_shortcut_accepts_cmd_v_only_on_macos() {
        let key = key_with('v', KeyModifiers::SUPER);
        assert_eq!(
            is_paste_shortcut(&key),
            cfg!(target_os = "macos"),
            "Cmd+V pastes on macOS only"
        );
    }

    #[test]
    fn test_paste_shortcut_rejects_other_keys_and_modifiers() {
        let wrong_key = key_with('c', KeyModifiers::CONTROL);
        assert!(!is_paste_shortcut(&wrong_key), "Ctrl+C is not paste");

        let bare_v = key_with('v', KeyModifiers::NONE);
        assert!(
            !is_paste_shortcut(&bare_v),
            "bare 'v' is typed text, not paste"
        );
    }

    // ── Copy ─────────────────────────────────────────────────────────

    #[test]
    fn test_copy_shortcut_on_macos_uses_cmd() {
        let cmd_c = key_with('c', KeyModifiers::SUPER);
        assert_eq!(
            is_copy_shortcut(&cmd_c),
            cfg!(target_os = "macos"),
            "Cmd+C copies on macOS only"
        );
    }

    #[test]
    fn test_copy_shortcut_off_macos_uses_ctrl() {
        let ctrl_c = key_with('c', KeyModifiers::CONTROL);
        assert_eq!(
            is_copy_shortcut(&ctrl_c),
            !cfg!(target_os = "macos"),
            "Ctrl+C copies on non-macOS only"
        );
    }

    // ── Cancel ───────────────────────────────────────────────────────

    #[test]
    fn test_cancel_shortcut_is_always_ctrl_c() {
        let ctrl_c = key_with('c', KeyModifiers::CONTROL);
        assert!(
            is_cancel_shortcut(&ctrl_c),
            "Ctrl+C cancels on every platform"
        );

        let cmd_c = key_with('c', KeyModifiers::SUPER);
        assert!(
            !is_cancel_shortcut(&cmd_c),
            "Cmd+C never cancels — it is the copy-only key"
        );
    }

    // ── Shortcut modifier ─────────────────────────────────────────────

    #[test]
    fn test_shortcut_modifier_accepts_ctrl_everywhere() {
        let ctrl_z = key_with('z', KeyModifiers::CONTROL);
        assert!(
            has_shortcut_modifier(&ctrl_z),
            "Ctrl+letter is always a chord"
        );
    }

    #[test]
    fn test_shortcut_modifier_accepts_cmd_only_on_macos() {
        let cmd_z = key_with('z', KeyModifiers::SUPER);
        assert_eq!(
            has_shortcut_modifier(&cmd_z),
            cfg!(target_os = "macos"),
            "Cmd+letter is a chord on macOS only — elsewhere it is plain text"
        );
    }

    #[test]
    fn test_shortcut_modifier_rejects_shift_and_bare_keys() {
        // Shift+letter is uppercase text, not a chord.
        let shift_z = key_with('z', KeyModifiers::SHIFT);
        assert!(!has_shortcut_modifier(&shift_z));

        let bare_z = key_with('z', KeyModifiers::NONE);
        assert!(!has_shortcut_modifier(&bare_z));
    }

    // ── Labels ───────────────────────────────────────────────────────

    #[test]
    fn test_labels_match_platform() {
        assert_eq!(cancel_shortcut_label(), "Ctrl+C");
        assert_eq!(
            copy_shortcut_label(),
            if cfg!(target_os = "macos") {
                "Cmd+C"
            } else {
                "Ctrl+C"
            }
        );
        assert_eq!(
            paste_shortcut_label(),
            if cfg!(target_os = "macos") {
                "Cmd+V"
            } else {
                "Ctrl+V"
            }
        );
    }
}
