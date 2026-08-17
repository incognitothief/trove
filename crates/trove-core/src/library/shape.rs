//! Read-only library shape inspection (ADR 007, Group E1).
//!
//! Walks a declared library root using only `readdir`/`stat` — no file
//! reads, no hashing — and reports structure: per-immediate-subdirectory
//! file counts, byte totals, and depth. Cheap even on a huge library because
//! it never opens file contents, which is what makes this safe to run
//! *before* committing to an actual import: "how is this actually going to
//! be scanned," visible up front instead of only becoming apparent from
//! stderr scroll partway through a real run. This is also the raw material
//! Group E2's Backfill Plan chunks against.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::import::{is_audio, is_hidden, DEFAULT_AUDIO_EXTENSIONS};
use crate::library::slug::compute_slug;

/// Stat-only totals for one subtree: an immediate child directory of the
/// library root, or the loose files sitting directly in the root itself.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtreeShape {
    /// Canonical slug (D2 format) of this subtree relative to the library
    /// root. `None` for files sitting directly in the root, outside any
    /// subdirectory — there's no folder to name.
    pub relative_path: Option<String>,
    pub audio_file_count: u64,
    pub audio_bytes: u64,
    pub total_file_count: u64,
    pub total_bytes: u64,
    /// Deepest file below this subtree, measured in directory levels from
    /// the library root (root itself is depth 0; a file directly in the
    /// root is depth 0; a file in an immediate subdirectory is depth 1).
    /// Kept on the same root-relative scale as [`LibraryShape::max_depth`]
    /// so subtrees are comparable to each other, not just to themselves.
    pub max_depth: u32,
}

/// The full read-only shape of a library root.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryShape {
    pub root: PathBuf,
    /// One entry per immediate subdirectory of the root, in alphabetical
    /// order by directory name — the same order Group E2 chunks against
    /// when it turns this into a Backfill Plan, so this order is
    /// load-bearing, not cosmetic.
    pub subtrees: Vec<SubtreeShape>,
    /// Files sitting directly in the root, outside any subdirectory.
    /// `None` if there are none.
    pub root_files: Option<SubtreeShape>,
    pub audio_file_count: u64,
    pub audio_bytes: u64,
    pub total_file_count: u64,
    pub total_bytes: u64,
    pub max_depth: u32,
}

/// Options controlling a shape scan. Mirrors [`crate::import::ImportOptions`]'s
/// `include_dotfiles` so a shape scan previews the same set of files an
/// actual import would see.
#[derive(Debug, Clone, Default)]
pub struct ShapeOptions {
    pub include_dotfiles: bool,
}

#[derive(Default)]
struct Accumulator {
    audio_file_count: u64,
    audio_bytes: u64,
    total_file_count: u64,
    total_bytes: u64,
    max_depth: u32,
}

impl Accumulator {
    fn add_file(&mut self, path: &Path, size: u64, depth: u32) {
        self.total_file_count += 1;
        self.total_bytes += size;
        if is_audio(path, DEFAULT_AUDIO_EXTENSIONS) {
            self.audio_file_count += 1;
            self.audio_bytes += size;
        }
        self.max_depth = self.max_depth.max(depth);
    }

    fn merge(&mut self, other: &Accumulator) {
        self.audio_file_count += other.audio_file_count;
        self.audio_bytes += other.audio_bytes;
        self.total_file_count += other.total_file_count;
        self.total_bytes += other.total_bytes;
        self.max_depth = self.max_depth.max(other.max_depth);
    }

    fn into_shape(self, relative_path: Option<String>) -> SubtreeShape {
        SubtreeShape {
            relative_path,
            audio_file_count: self.audio_file_count,
            audio_bytes: self.audio_bytes,
            total_file_count: self.total_file_count,
            total_bytes: self.total_bytes,
            max_depth: self.max_depth,
        }
    }
}

/// Scan `root` and report its shape. `readdir` + `stat` only — never opens
/// a file's contents.
pub fn scan_library_shape(root: &Path, options: &ShapeOptions) -> Result<LibraryShape> {
    if !root.is_dir() {
        return Err(Error::not_found(format!(
            "library root not found or not a directory: {}",
            root.display()
        )));
    }

    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();

    let mut root_files_acc = Accumulator::default();
    let mut child_dirs = Vec::new();
    for path in entries {
        if is_hidden(&path, options.include_dotfiles) {
            continue;
        }
        if path.is_dir() {
            child_dirs.push(path);
        } else if path.is_file() {
            let size = std::fs::metadata(&path)?.len();
            root_files_acc.add_file(&path, size, 0);
        }
    }

    let mut subtrees = Vec::with_capacity(child_dirs.len());
    let mut totals = Accumulator::default();
    for dir in &child_dirs {
        let mut acc = Accumulator::default();
        walk_subtree(dir, 1, options.include_dotfiles, &mut acc)?;
        totals.merge(&acc);
        let relative_path = compute_slug(root, dir);
        subtrees.push(acc.into_shape(relative_path));
    }

    let root_files = if root_files_acc.total_file_count > 0 {
        totals.merge(&root_files_acc);
        Some(root_files_acc.into_shape(None))
    } else {
        None
    };

    Ok(LibraryShape {
        root: root.to_path_buf(),
        subtrees,
        root_files,
        audio_file_count: totals.audio_file_count,
        audio_bytes: totals.audio_bytes,
        total_file_count: totals.total_file_count,
        total_bytes: totals.total_bytes,
        max_depth: totals.max_depth,
    })
}

fn walk_subtree(dir: &Path, depth: u32, include_dotfiles: bool, acc: &mut Accumulator) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_hidden(&path, include_dotfiles) {
            continue;
        }
        if path.is_dir() {
            walk_subtree(&path, depth + 1, include_dotfiles, acc)?;
        } else if path.is_file() {
            let size = std::fs::metadata(&path)?.len();
            acc.add_file(&path, size, depth);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn reports_per_subtree_counts_bytes_and_depth_without_reading_audio_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Artist A/Track 1.mp3", b"12345");
        write(root, "Artist A/Track 2.flac", b"1234567");
        write(root, "Artist B/Sub/Deep.mp3", b"123");
        write(root, "Artist B/cover.jpg", b"not audio, longer bytes");

        let shape = scan_library_shape(root, &ShapeOptions::default()).unwrap();

        assert_eq!(shape.subtrees.len(), 2);

        let artist_a = shape
            .subtrees
            .iter()
            .find(|s| s.relative_path.as_deref() == Some("Artist A"))
            .unwrap();
        assert_eq!(artist_a.audio_file_count, 2);
        assert_eq!(artist_a.audio_bytes, 5 + 7);
        assert_eq!(artist_a.total_file_count, 2);
        assert_eq!(artist_a.max_depth, 1);

        let artist_b = shape
            .subtrees
            .iter()
            .find(|s| s.relative_path.as_deref() == Some("Artist B"))
            .unwrap();
        assert_eq!(artist_b.audio_file_count, 1);
        assert_eq!(artist_b.total_file_count, 2, "cover.jpg counts toward total, not audio");
        assert_eq!(artist_b.max_depth, 2, "Deep.mp3 is two levels below the root");

        assert_eq!(shape.audio_file_count, 3);
        assert_eq!(shape.total_file_count, 4);
        assert_eq!(shape.max_depth, 2);
        assert!(shape.root_files.is_none());
    }

    #[test]
    fn tracks_loose_files_sitting_directly_in_the_root_separately_from_subtrees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "loose.mp3", b"12345");
        write(root, "Artist/Track.mp3", b"123");

        let shape = scan_library_shape(root, &ShapeOptions::default()).unwrap();

        let root_files = shape.root_files.expect("loose.mp3 sits directly in root");
        assert_eq!(root_files.relative_path, None);
        assert_eq!(root_files.audio_file_count, 1);
        assert_eq!(root_files.max_depth, 0);
        assert_eq!(shape.subtrees.len(), 1);
        assert_eq!(shape.audio_file_count, 2);
    }

    #[test]
    fn dotfiles_are_excluded_by_default_and_included_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Artist/Track.mp3", b"123");
        write(root, "Artist/.hidden.mp3", b"12345");
        write(root, ".DS_Store", b"junk");

        let default_shape = scan_library_shape(root, &ShapeOptions::default()).unwrap();
        assert_eq!(default_shape.audio_file_count, 1);
        assert!(default_shape.root_files.is_none());

        let with_dotfiles = scan_library_shape(
            root,
            &ShapeOptions {
                include_dotfiles: true,
            },
        )
        .unwrap();
        assert_eq!(with_dotfiles.audio_file_count, 2);
        assert!(with_dotfiles.root_files.is_some());
    }

    #[test]
    fn refuses_a_root_that_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(scan_library_shape(&missing, &ShapeOptions::default()).is_err());
    }

    #[test]
    fn empty_root_reports_zeroed_totals_and_no_subtrees() {
        let dir = tempfile::tempdir().unwrap();
        let shape = scan_library_shape(dir.path(), &ShapeOptions::default()).unwrap();
        assert!(shape.subtrees.is_empty());
        assert!(shape.root_files.is_none());
        assert_eq!(shape.total_file_count, 0);
        assert_eq!(shape.max_depth, 0);
    }
}
