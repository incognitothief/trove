//! SQL schema definitions for the local SQLite databases described in ADR 000.
//!
//! Each constant is idempotent (`CREATE TABLE IF NOT EXISTS ...`) so opening a
//! database is always safe and rebuildable — the local cache is disposable.

/// `~/.trove/archive.sqlite` — disposable local mirror of the bucket index.
pub const ARCHIVE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tracks (
    track_id             TEXT PRIMARY KEY,
    object_key           TEXT NOT NULL,
    size_bytes           INTEGER NOT NULL,
    sha256               TEXT NOT NULL,
    title                TEXT,
    artist               TEXT,
    album                TEXT,
    genre                TEXT,
    year                 INTEGER,
    bpm                  REAL,
    key_camelot          TEXT,
    duration_secs        REAL,
    comment              TEXT,
    file_type            TEXT,
    tags                 TEXT,           -- JSON array
    imported_at          TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    source_path_original TEXT,
    artwork_object_key   TEXT
);

CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
CREATE INDEX IF NOT EXISTS idx_tracks_genre  ON tracks(genre);
CREATE INDEX IF NOT EXISTS idx_tracks_bpm    ON tracks(bpm);
CREATE INDEX IF NOT EXISTS idx_tracks_key    ON tracks(key_camelot);
CREATE INDEX IF NOT EXISTS idx_tracks_sha256 ON tracks(sha256);
"#;

/// `~/.trove/playlists.sqlite` — logical Trove playlists/crates.
pub const PLAYLISTS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS playlists (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS playlist_tracks (
    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    TEXT NOT NULL,
    position    INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, track_id)
);
"#;

/// `~/.trove/volumes/{volume_id}.sqlite` — last-known state of a volume.
pub const VOLUME_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS volume (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
    track_id      TEXT PRIMARY KEY,
    relative_path TEXT NOT NULL,
    size_bytes    INTEGER,
    mtime         TEXT,
    sha256        TEXT,
    status        TEXT NOT NULL,        -- copied | pending | stale | failed
    last_seen_at  TEXT
);
"#;

/// `~/.trove/sync.sqlite` — transfer state + bulk-import bookkeeping.
pub const SYNC_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS transfers (
    id           TEXT PRIMARY KEY,
    track_id     TEXT NOT NULL,
    direction    TEXT NOT NULL,         -- download | upload
    object_key   TEXT NOT NULL,
    local_path   TEXT,
    bytes_total  INTEGER,
    bytes_done   INTEGER NOT NULL DEFAULT 0,
    status       TEXT NOT NULL,         -- pending | active | done | failed
    attempts     INTEGER NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS import_jobs (
    id             TEXT PRIMARY KEY,
    source_root    TEXT NOT NULL,
    phase          TEXT NOT NULL,       -- scan|fingerprint|dedupe|upload|verify|commit|done
    staging_prefix TEXT NOT NULL,
    total_files    INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS import_files (
    job_id             TEXT NOT NULL REFERENCES import_jobs(id) ON DELETE CASCADE,
    path               TEXT NOT NULL,
    size               INTEGER,
    mtime              TEXT,
    sha256             TEXT,
    metadata_extracted INTEGER NOT NULL DEFAULT 0,
    state              TEXT NOT NULL,   -- see import::state::FileState
    s3_object_key      TEXT,
    etag               TEXT,
    error              TEXT,
    PRIMARY KEY (job_id, path)
);
"#;
