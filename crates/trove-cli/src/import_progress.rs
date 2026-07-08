//! Terminal progress rendering for long-running import commands.

use std::io::{self, Write};

use trove_core::import::{ImportProgress, ImportProgressEvent, ImportProgressKind};

/// Renders import progress to stderr (human mode only).
pub struct CliImportProgress;

impl CliImportProgress {
    pub fn new() -> Self {
        CliImportProgress
    }

    fn format_elapsed(secs: f64) -> String {
        let total = secs.round() as u64;
        format!("{}:{:02}", total / 60, total % 60)
    }
}

impl ImportProgress for CliImportProgress {
    fn on_progress(&mut self, event: &ImportProgressEvent) {
        match event.kind {
            ImportProgressKind::JobCreated => {
                let _ = writeln!(io::stderr(), "job_id: {}", event.job_id);
            }
            ImportProgressKind::PhaseStart => {
                let _ = writeln!(
                    io::stderr(),
                    "{}: {} file(s)",
                    event.phase,
                    event.files_total
                );
            }
            ImportProgressKind::FileDone => {
                let path = event
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "?".into());
                let _ = writeln!(
                    io::stderr(),
                    "  {:>4}/{}  {:>5}  {}",
                    event.files_done,
                    event.files_total,
                    Self::format_elapsed(event.elapsed_secs),
                    path
                );
            }
            ImportProgressKind::PhaseDone => {}
        }
    }
}

impl Default for CliImportProgress {
    fn default() -> Self {
        Self::new()
    }
}
