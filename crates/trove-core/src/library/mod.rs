//! The declared library root and everything anchored to it: portable
//! cross-drive identity (the slug, ADR 007 Group D2), the read-only shape
//! scan (Group E1), and the backfill Plan (Group E2). Separate from
//! `import`, which is scoped to a single job's own source path, not the
//! stable, explicitly-declared root these features need instead.

pub mod slug;

pub use slug::compute_slug;
