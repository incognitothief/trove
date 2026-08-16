//! Canonical, portable "path relative to the library root" identity (ADR
//! 007, Group D2).
//!
//! This is what lets Trove recognize "the same library, remounted somewhere
//! else" (a replacement drive, a clone via `rsync`) cheaply — but only if
//! the same logical relative path always produces the exact same string,
//! regardless of host OS or filesystem quirks. Two real gotchas make this
//! non-trivial: Windows uses `\` where macOS/Linux use `/`, and some
//! filesystems (notably HFS+/APFS for accented characters) hand back
//! NFD-decomposed Unicode where most other systems produce NFC-composed
//! forms. Get either wrong and the slug silently fails to match — not a
//! correctness bug (the fast path just misses and falls back to a full
//! hash), but exactly the cost this feature exists to avoid.

use std::path::{Component, Path};

use unicode_normalization::UnicodeNormalization;

/// Compute the canonical slug for `path` relative to `library_root`.
///
/// Returns `None` if `path` isn't actually under `library_root`, or if the
/// relative portion is empty or contains anything other than plain named
/// components (`..`, a root, a prefix) — none of which should appear in a
/// real discovered file path, but this stays defensive rather than
/// producing a misleading slug for an unexpected input.
///
/// Canonical form: forward-slash separated regardless of host OS, no
/// leading slash, NFC-normalized.
pub fn compute_slug(library_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(library_root).ok()?;

    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }

    let joined = parts.join("/");
    Some(joined.nfc().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_relative_slug_under_the_library_root() {
        let root = Path::new("/Volumes/T7/music/library");
        let path = Path::new("/Volumes/T7/music/library/Theo Parrish/Sound Signature.flac");
        assert_eq!(
            compute_slug(root, path).as_deref(),
            Some("Theo Parrish/Sound Signature.flac")
        );
    }

    #[test]
    fn same_relative_structure_under_a_different_root_produces_the_same_slug() {
        // The whole point: T7 -> T72, same catalog position, different mount.
        let slug_a = compute_slug(
            Path::new("/Volumes/T7/music/library"),
            Path::new("/Volumes/T7/music/library/Theo Parrish/Sound Signature.flac"),
        );
        let slug_b = compute_slug(
            Path::new("/Volumes/T72/choons"),
            Path::new("/Volumes/T72/choons/Theo Parrish/Sound Signature.flac"),
        );
        assert_eq!(slug_a, slug_b);
    }

    #[test]
    fn returns_none_when_path_is_not_under_the_root() {
        let root = Path::new("/Volumes/T7/music/library");
        let path = Path::new("/Volumes/T7/other-stuff/track.mp3");
        assert_eq!(compute_slug(root, path), None);
    }

    #[test]
    fn nfd_and_nfc_forms_of_the_same_name_produce_the_same_slug() {
        // "é" as a single precomposed codepoint (NFC) vs "e" + combining
        // acute accent (NFD) — the exact macOS/APFS gotcha this exists for.
        let nfc_name = "Cafe\u{301}.mp3"; // "Café.mp3", combining accent (NFD-ish input)
        let root = Path::new("/lib");
        let path_nfd = Path::new("/lib").join(nfc_name);
        let path_nfc = Path::new("/lib/Café.mp3"); // precomposed é (U+00E9)

        let slug_from_nfd_input = compute_slug(root, &path_nfd).unwrap();
        let slug_from_nfc_input = compute_slug(root, path_nfc).unwrap();
        assert_eq!(slug_from_nfd_input, slug_from_nfc_input);
        // And confirm it actually landed on the composed form, not just that
        // both inputs happened to agree.
        assert_eq!(slug_from_nfd_input, "Café.mp3");
    }

    #[test]
    fn never_produces_a_leading_slash() {
        let root = Path::new("/lib");
        let path = Path::new("/lib/track.mp3");
        let slug = compute_slug(root, path).unwrap();
        assert!(!slug.starts_with('/'), "slug was {slug:?}");
    }
}
