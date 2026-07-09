//! Audit trail — records every shell execution attempt.
//!
//! Writes newline-delimited JSON to `.loomis/audit.jsonl`.  A small
//! in-memory ring buffer holds the most recent entries so the TUI can
//! display them without re-reading the file.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use tools::SandboxConfig;

/// Append-only JSONL audit log.
pub struct AuditLogger {
    enabled: bool,
    file: Option<Mutex<File>>,
    ring: Mutex<VecDeque<AuditEntry>>,
    max_ring: usize,
}

/// A single audit record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub session_id: String,
    pub tool: String,
    pub command: String,
    pub verdict: String,
    pub outcome: String,
}

impl AuditLogger {
    pub fn new(config: &SandboxConfig, workspace_root: &Path) -> Self {
        let file = if config.audit.enabled {
            let log_path = workspace_root.join(&config.audit.log_file);
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .ok()
                .map(Mutex::new)
        } else {
            None
        };

        Self {
            enabled: config.audit.enabled,
            file,
            ring: Mutex::new(VecDeque::with_capacity(256)),
            max_ring: 256,
        }
    }

    pub fn log(&self, entry: AuditEntry) {
        if !self.enabled {
            return;
        }

        // In-memory ring buffer
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= self.max_ring {
                ring.pop_front();
            }
            ring.push_back(entry.clone());
        }

        // Append to file
        if let Some(ref file_mutex) = self.file
            && let Ok(mut file) = file_mutex.lock()
            && let Ok(json) = serde_json::to_string(&entry)
        {
            let _ = writeln!(file, "{json}");
        }
    }

    /// Return recent audit entries (for display via `/audit`).
    pub fn recent(&self, count: usize) -> Vec<AuditEntry> {
        if let Ok(ring) = self.ring.lock() {
            ring.iter().rev().take(count).cloned().collect()
        } else {
            vec![]
        }
    }
}
