//! The declared library root and everything anchored to it: portable
//! cross-drive identity (the slug, ADR 007 Group D2), the read-only shape
//! scan (Group E1), and the backfill Plan (Group E2). Separate from
//! `import`, which is scoped to a single job's own source path, not the
//! stable, explicitly-declared root these features need instead.

pub mod backfill;
pub mod events;
pub mod plan;
pub mod shape;
pub mod slug;

pub use backfill::{plan_slug_backfill, BackfillSlugsReport};
pub use events::{fold_chunk_status, ChunkEvent, ChunkEventKind, ChunkState, ChunkStatus};
pub use plan::{build_plan, BackfillPlan, PlanChunk};
pub use shape::{scan_library_shape, LibraryShape, ShapeOptions, SubtreeShape};
pub use slug::compute_slug;
