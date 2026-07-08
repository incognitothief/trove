//! `trove` — a thin CLI over `trove-core`.
//!
//! Every subcommand translates flags into a single core call and renders the
//! result. It contains no reconcile/query/sync logic of its own.

mod runtime;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use trove_core::model::TrackId;
use trove_core::query::{QuerySpec, Range};

#[derive(Parser)]
#[command(
    name = "trove",
    version,
    about = "Portable DJ library recovery and export"
)]
struct Cli {
    /// Emit machine-readable JSON instead of human text.
    #[arg(long, global = true)]
    json: bool,

    /// Serve reads from the last cached index if the bucket is unreachable.
    #[arg(long, global = true)]
    offline: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the canonical archive index in the bucket.
    #[command(subcommand)]
    Archive(ArchiveCmd),
    /// Import new content into the durable archive.
    Import(ImportArgs),
    /// Search the archive (reconciles with the bucket first).
    Query(QueryArgs),
    /// Manage logical playlists/crates.
    #[command(subcommand)]
    Playlist(PlaylistCmd),
    /// Manage removable performance volumes.
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Flash/sync tracks to a mounted volume.
    #[command(subcommand)]
    Sync(SyncCmd),
}

#[derive(Subcommand)]
enum ArchiveCmd {
    /// Reconcile the local cache with the bucket index.
    PullIndex,
    /// Push the local archive index up as the new canonical generation.
    PushIndex,
    /// Verify the archive index against stored objects.
    Verify,
}

#[derive(Args)]
struct ImportArgs {
    /// Source folder to import.
    path: String,
    /// Only scan/hash/dedupe and print a plan (no upload).
    #[arg(long)]
    plan: bool,
    /// Treat as a resumable bulk migration.
    #[arg(long)]
    bulk: bool,
    /// Resume a previously interrupted job.
    #[arg(long)]
    resume: bool,
}

#[derive(Args)]
struct QueryArgs {
    #[arg(long)]
    text: Option<String>,
    #[arg(long)]
    artist: Option<String>,
    #[arg(long)]
    album: Option<String>,
    #[arg(long)]
    genre: Option<String>,
    #[arg(long)]
    key: Option<String>,
    /// BPM range as `min:max`, e.g. `118:124` (either side optional).
    #[arg(long)]
    bpm: Option<String>,
    #[arg(long)]
    limit: Option<u32>,
}

#[derive(Subcommand)]
enum PlaylistCmd {
    /// Create a new empty playlist.
    Create { name: String },
    /// Add tracks (by id) to a playlist.
    Add {
        name: String,
        track_ids: Vec<String>,
    },
    /// Remove a track from a playlist.
    Remove { name: String, track_id: String },
    /// List all playlists.
    List,
    /// Export a playlist to a portable file.
    Export {
        name: String,
        #[arg(long, default_value = "m3u8")]
        format: String,
    },
}

#[derive(Subcommand)]
enum VolumeCmd {
    /// Initialize a volume layout + identity file at a mount point.
    Init {
        mount_point: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Show a volume's identity.
    Status { mount_point: String },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Plan (and, once implemented, run) a sync of a playlist to a volume.
    Playlist {
        name: String,
        #[arg(long)]
        to: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    if let Err(err) = run(&cli) {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Archive(cmd) => archive(cli, cmd),
        Command::Import(args) => import(cli, args),
        Command::Query(args) => query(cli, args),
        Command::Playlist(cmd) => playlist(cli, cmd),
        Command::Volume(cmd) => volume(cli, cmd),
        Command::Sync(cmd) => sync(cli, cmd),
    }
}

fn archive(cli: &Cli, cmd: &ArchiveCmd) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    match cmd {
        ArchiveCmd::PullIndex => {
            let report = trove.reconcile(cli.offline)?;
            println!("{report:?}");
        }
        ArchiveCmd::PushIndex => {
            let generation = trove.push_index()?;
            println!("pushed canonical index at generation {generation}");
        }
        ArchiveCmd::Verify => {
            anyhow::bail!("archive verify is not implemented in this bootstrap yet");
        }
    }
    Ok(())
}

fn import(_cli: &Cli, args: &ImportArgs) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    let source = std::path::Path::new(&args.path);
    let mut job = trove
        .import_plan(source)
        .with_context(|| format!("planning import of {}", args.path))?;
    let stats = job.stats();
    println!(
        "planned job {}: {} file(s), {} duplicate(s)",
        job.id, stats.total, stats.duplicates
    );

    if args.plan {
        for file in &job.files {
            println!("  {:>9}  {}", file.state.as_str(), file.path.display());
        }
        return Ok(());
    }

    let committed = trove.import_run(&mut job).context("running import")?;
    println!("committed {committed} track(s) into the archive");
    Ok(())
}

fn query(cli: &Cli, args: &QueryArgs) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    let (bpm_min, bpm_max) = parse_range(args.bpm.as_deref())?;
    let spec = QuerySpec {
        text: args.text.clone(),
        artist: args.artist.clone(),
        album: args.album.clone(),
        genre: args.genre.clone(),
        key: args.key.clone(),
        file_type: None,
        bpm: Range {
            min: bpm_min,
            max: bpm_max,
        },
        year: Range::default(),
        tags: Vec::new(),
        limit: args.limit,
    };

    let results = trove.query(&spec, cli.offline)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }
    if results.is_empty() {
        println!("no matches");
        return Ok(());
    }
    for entry in &results {
        println!(
            "{}  {} - {} [{}{}]",
            entry.track_id,
            entry.metadata.artist.as_deref().unwrap_or("?"),
            entry.metadata.title.as_deref().unwrap_or("?"),
            entry
                .metadata
                .bpm
                .map(|b| format!("{b:.0} BPM "))
                .unwrap_or_default(),
            entry.metadata.key.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

fn playlist(cli: &Cli, cmd: &PlaylistCmd) -> Result<()> {
    let trove = runtime::open_trove()?;
    match cmd {
        PlaylistCmd::Create { name } => {
            let pl = trove.playlist_create(name)?;
            println!("created playlist '{}' ({})", pl.name, pl.id);
        }
        PlaylistCmd::Add { name, track_ids } => {
            let ids: Vec<TrackId> = track_ids.iter().map(|s| TrackId(s.clone())).collect();
            trove.playlist_add(name, &ids)?;
            println!("added {} track(s) to '{name}'", ids.len());
        }
        PlaylistCmd::Remove { name, track_id } => {
            trove.playlist_remove(name, &TrackId(track_id.clone()))?;
            println!("removed {track_id} from '{name}'");
        }
        PlaylistCmd::List => {
            let playlists = trove.playlist_list()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&playlists)?);
            } else if playlists.is_empty() {
                println!("no playlists");
            } else {
                for pl in playlists {
                    println!("{}  {}", pl.id, pl.name);
                }
            }
        }
        PlaylistCmd::Export { name, format } => {
            let _ = trove.playlist_get(name)?;
            anyhow::bail!("playlist export ({format}) is not implemented in this bootstrap yet");
        }
    }
    Ok(())
}

fn volume(cli: &Cli, cmd: &VolumeCmd) -> Result<()> {
    use trove_core::volume;
    match cmd {
        VolumeCmd::Init { mount_point, label } => {
            let identity = volume::init(std::path::Path::new(mount_point), label.clone())?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&identity)?);
            } else {
                println!("initialized volume {} at {mount_point}", identity.volume_id);
            }
        }
        VolumeCmd::Status { mount_point } => {
            match volume::read_identity(std::path::Path::new(mount_point))? {
                Some(identity) => println!(
                    "volume {} (label: {})",
                    identity.volume_id,
                    identity.label.as_deref().unwrap_or("-")
                ),
                None => println!("no Trove identity found at {mount_point}"),
            }
        }
    }
    Ok(())
}

fn sync(cli: &Cli, cmd: &SyncCmd) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    match cmd {
        SyncCmd::Playlist { name, to } => {
            let plan = trove.plan_playlist_sync(name, cli.offline)?;
            println!(
                "sync plan for '{name}' -> {to}: {} transfer(s), {} byte(s) remaining",
                plan.transfers.len(),
                plan.bytes_remaining
            );
            for t in &plan.transfers {
                println!("  {} -> {}", t.object_key, t.relative_path);
            }
            eprintln!("note: transfer execution is not implemented in this bootstrap yet");
        }
    }
    Ok(())
}

/// Parse a `min:max` range where either side may be empty.
fn parse_range(spec: Option<&str>) -> Result<(Option<f32>, Option<f32>)> {
    let Some(spec) = spec else {
        return Ok((None, None));
    };
    let (min, max) = spec
        .split_once(':')
        .with_context(|| format!("range '{spec}' must be in min:max form"))?;
    let parse = |s: &str| -> Result<Option<f32>> {
        if s.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(
                s.trim().parse().with_context(|| format!("parsing '{s}'"))?,
            ))
        }
    };
    Ok((parse(min)?, parse(max)?))
}
