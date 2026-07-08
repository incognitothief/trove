//! Import progress hooks for CLI, daemon, and UI clients.
//!
//! Long-running import steps emit structured [`ImportProgressEvent`]s through an
//! optional [`ImportProgress`] listener. Clients render them however they like
//! (terminal status line, WebSocket push, etc.).

use std::path::PathBuf;
use std::time::Instant;

use serde::Serialize;

use super::{FileState, Phase};

/// What triggered a progress event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportProgressKind {
    JobCreated,
    PhaseStart,
    FileDone,
    PhaseDone,
}

/// A single import progress update.
#[derive(Debug, Clone, Serialize)]
pub struct ImportProgressEvent {
    pub kind: ImportProgressKind,
    pub phase: Phase,
    pub job_id: String,
    pub files_done: usize,
    pub files_total: usize,
    /// Wall time since the current import step began.
    pub elapsed_secs: f64,
    pub path: Option<PathBuf>,
    pub file_state: Option<FileState>,
}

/// Listener for import progress (CLI, HTTP stream, UI, tests).
pub trait ImportProgress {
    fn on_progress(&mut self, event: &ImportProgressEvent);
}

/// Default no-op listener when progress output is disabled.
#[derive(Debug, Default)]
pub struct NoopImportProgress;

impl ImportProgress for NoopImportProgress {
    fn on_progress(&mut self, _event: &ImportProgressEvent) {}
}

/// Tracks elapsed time and forwards events to an optional listener.
pub struct ProgressCtx<'a> {
    listener: Option<&'a mut dyn ImportProgress>,
    started: Instant,
    job_id: String,
}

impl<'a> ProgressCtx<'a> {
    pub fn new(job_id: impl Into<String>, listener: Option<&'a mut dyn ImportProgress>) -> Self {
        ProgressCtx {
            listener,
            started: Instant::now(),
            job_id: job_id.into(),
        }
    }

    pub fn job_created(&mut self) {
        // This is emitted once at the beginning of an import transaction so
        // CLI users can copy/resume the job id immediately.
        self.emit(ImportProgressEvent {
            kind: ImportProgressKind::JobCreated,
            phase: Phase::Fingerprint,
            job_id: self.job_id.clone(),
            files_done: 0,
            files_total: 0,
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            path: None,
            file_state: None,
        });
    }

    pub fn phase_start(&mut self, phase: Phase, files_total: usize) {
        self.emit(ImportProgressEvent {
            kind: ImportProgressKind::PhaseStart,
            phase,
            job_id: self.job_id.clone(),
            files_done: 0,
            files_total,
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            path: None,
            file_state: None,
        });
    }

    pub fn file_done(
        &mut self,
        phase: Phase,
        files_done: usize,
        files_total: usize,
        path: &std::path::Path,
        file_state: FileState,
    ) {
        self.emit(ImportProgressEvent {
            kind: ImportProgressKind::FileDone,
            phase,
            job_id: self.job_id.clone(),
            files_done,
            files_total,
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            path: Some(path.to_path_buf()),
            file_state: Some(file_state),
        });
    }

    pub fn phase_done(&mut self, phase: Phase, files_done: usize, files_total: usize) {
        self.emit(ImportProgressEvent {
            kind: ImportProgressKind::PhaseDone,
            phase,
            job_id: self.job_id.clone(),
            files_done,
            files_total,
            elapsed_secs: self.started.elapsed().as_secs_f64(),
            path: None,
            file_state: None,
        });
    }

    fn emit(&mut self, event: ImportProgressEvent) {
        if let Some(listener) = self.listener.as_deref_mut() {
            listener.on_progress(&event);
        }
    }
}
