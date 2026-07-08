//! `trove` — a thin CLI over `trove-core`.
//!
//! Every subcommand translates flags into a single core call and renders the
//! result. It contains no reconcile/query/sync logic of its own.

mod import_progress;
mod runtime;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
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
    #[command(subcommand)]
    command: Option<ImportCmd>,
    /// Source folder for one-shot import when no subcommand is given.
    #[arg(value_name = "PATH")]
    path: Option<String>,
    #[command(flatten)]
    options: ImportOptionsArgs,
}

#[derive(Subcommand)]
enum ImportCmd {
    /// Scan/hash/dedupe and persist a job (dry-run friendly).
    Plan {
        path: String,
        #[command(flatten)]
        options: ImportOptionsArgs,
    },
    /// Upload + verify staged objects (no commit).
    Run { job_id: String },
    /// Re-verify staged objects.
    Verify { job_id: String },
    /// Promote verified objects and advance the archive index.
    Commit { job_id: String },
    /// Continue upload + verify from the last safe state.
    Resume { job_id: String },
    /// Per-phase / per-state counts for a job.
    Status { job_id: String },
    /// List import jobs (incomplete by default).
    List {
        /// Include finished jobs.
        #[arg(long)]
        all: bool,
    },
    /// Drop local import job state (sync.sqlite + manifest). Does not delete bucket objects.
    Prune { job_id: String },
}

#[derive(Args, Default)]
struct ImportOptionsArgs {
    /// Include dotfiles / hidden directories (excluded by default).
    #[arg(long)]
    include_dotfiles: bool,
    /// Do not capture co-located cover art (captured by default for directories).
    #[arg(long)]
    no_artwork: bool,
    /// Cover-art image or folder to import (`--artwork cover.jpg`).
    #[arg(long, value_name = "PATH")]
    artwork: Vec<PathBuf>,
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
    /// Show a volume's identity and file stats.
    Status { mount_point: String },
    /// List known volumes (host-side).
    List,
    /// Compare a playlist or query against a volume without transferring.
    Diff {
        mount_point: String,
        #[arg(long)]
        playlist: Option<String>,
        #[command(flatten)]
        query: QueryArgs,
    },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Sync a playlist to a volume.
    Playlist {
        name: String,
        #[arg(long)]
        to: String,
        /// Print the plan without transferring.
        #[arg(long)]
        plan: bool,
    },
    /// Sync query results to a volume.
    Query {
        #[arg(long)]
        to: String,
        #[arg(long)]
        plan: bool,
        #[command(flatten)]
        filters: QueryArgs,
    },
    /// Continue the latest active sync job.
    Resume,
    /// Verify tracks on a volume.
    Verify {
        mount_point: String,
        #[arg(long)]
        playlist: Option<String>,
        #[command(flatten)]
        query: QueryArgs,
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

fn import(cli: &Cli, args: &ImportArgs) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    let mut cli_progress = import_progress::CliImportProgress::new();
    let mut noop = trove_core::NoopImportProgress;
    let progress: &mut dyn trove_core::import::ImportProgress = if cli.json {
        &mut noop
    } else {
        &mut cli_progress
    };

    if args.command.is_none() {
        let path = args
            .path
            .as_deref()
            .context("import requires a source path or subcommand")?;
        let opts = merge_import_options(&trove, &args.options);
        let source = std::path::Path::new(path);
        let (job, committed) = trove
            .import_run_full(source, &opts, progress)
            .with_context(|| format!("importing {path}"))?;
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "job_id": job.id,
                    "committed": committed,
                    "artwork_captured": job.artwork.len(),
                }))?
            );
        } else {
            println!(
                "job {}: committed {committed} track(s), captured {} cover-art object(s)",
                job.id,
                job.artwork.len()
            );
        }
        return Ok(());
    }

    match args.command.as_ref().unwrap() {
        ImportCmd::Plan { path, options } => {
            let opts = merge_import_options(&trove, options);
            let source = std::path::Path::new(path);
            let job = trove
                .import_plan(source, &opts, progress)
                .with_context(|| format!("planning import of {path}"))?;
            print_planned_job(&job, cli.json);
        }
        ImportCmd::Run { job_id } => {
            let job = trove
                .import_run_job(job_id, progress)
                .with_context(|| format!("running import job {job_id}"))?;
            print_run_result(&job, cli.json);
        }
        ImportCmd::Verify { job_id } => {
            let job = trove
                .import_verify_job(job_id, progress)
                .with_context(|| format!("verifying import job {job_id}"))?;
            print_run_result(&job, cli.json);
        }
        ImportCmd::Commit { job_id } => {
            let committed = trove
                .import_commit_job(job_id, progress)
                .with_context(|| format!("committing import job {job_id}"))?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "job_id": job_id,
                        "committed": committed,
                    }))?
                );
            } else {
                println!("committed {committed} track(s) for job {job_id}");
            }
        }
        ImportCmd::Resume { job_id } => {
            let job = trove
                .import_resume(job_id, progress)
                .with_context(|| format!("resuming import job {job_id}"))?;
            print_run_result(&job, cli.json);
            if !cli.json {
                eprintln!("run `trove import commit {job_id}` when ready to promote");
            }
        }
        ImportCmd::Status { job_id } => {
            let status = trove
                .import_status(job_id)
                .with_context(|| format!("status for import job {job_id}"))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "job {}  phase={}  total={} committed={} failed={} duplicates={}",
                    status.id,
                    status.phase,
                    status.stats.total,
                    status.stats.committed,
                    status.stats.failed,
                    status.stats.duplicates,
                );
            }
        }
        ImportCmd::List { all } => {
            let jobs = trove.import_list(*all)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
            } else if jobs.is_empty() {
                println!("no import jobs");
            } else {
                for job in jobs {
                    println!(
                        "{}  {}  phase={}  files={} committed={} failed={}  updated={}",
                        job.id,
                        job.source_root.display(),
                        job.phase,
                        job.total_files,
                        job.stats.committed,
                        job.stats.failed,
                        job.updated_at,
                    );
                }
            }
        }
        ImportCmd::Prune { job_id } => {
            trove
                .import_prune(job_id)
                .with_context(|| format!("pruning import job {job_id}"))?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "pruned": job_id,
                    }))?
                );
            } else {
                println!("pruned import job {job_id}");
            }
        }
    }
    Ok(())
}

fn merge_import_options(
    trove: &trove_core::Trove,
    args: &ImportOptionsArgs,
) -> trove_core::ImportOptions {
    trove_core::ImportOptions {
        include_dotfiles: trove.config.import.include_dotfiles || args.include_dotfiles,
        capture_artwork: trove.config.import.capture_artwork && !args.no_artwork,
        artwork_paths: args.artwork.clone(),
    }
}

fn print_planned_job(job: &trove_core::import::ImportJob, json: bool) {
    let stats = job.stats();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "job_id": job.id,
                "total": stats.total,
                "duplicates": stats.duplicates,
                "artwork_candidates": job.artwork.len(),
                "files": job.files.iter().map(|f| serde_json::json!({
                    "path": f.path,
                    "state": f.state.as_str(),
                    "sha256": f.sha256,
                })).collect::<Vec<_>>(),
            }))
            .expect("serialize plan")
        );
        return;
    }
    println!(
        "planned job {}: {} file(s), {} duplicate(s), {} cover-art candidate(s)",
        job.id,
        stats.total,
        stats.duplicates,
        job.artwork.len()
    );
    for file in &job.files {
        println!("  {:>9}  {}", file.state.as_str(), file.path.display());
    }
    for art in &job.artwork {
        println!("  {:>9}  {}", "artwork", art.path.display());
    }
}

fn print_run_result(job: &trove_core::import::ImportJob, json: bool) {
    let stats = job.stats();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "job_id": job.id,
                "phase": job.phase.as_str(),
                "stats": stats,
            }))
            .expect("serialize run result")
        );
        return;
    }
    println!(
        "job {} phase={} verified={} failed={}",
        job.id, job.phase, stats.verified, stats.failed
    );
}

fn query(cli: &Cli, args: &QueryArgs) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    let spec = query_to_spec(args)?;
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
    let mut trove = runtime::open_trove()?;
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
        PlaylistCmd::Export { name, format: _ } => {
            let export = trove.playlist_export(name, cli.offline)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&export)?);
            } else {
                print!("{}", export.body);
            }
        }
    }
    Ok(())
}

fn volume(cli: &Cli, cmd: &VolumeCmd) -> Result<()> {
    use trove_core::volume;
    let mut trove = runtime::open_trove()?;
    match cmd {
        VolumeCmd::Init { mount_point, label } => {
            let identity = volume::init(
                std::path::Path::new(mount_point),
                label.clone(),
                Some(&trove.home),
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&identity)?);
            } else {
                println!("initialized volume {} at {mount_point}", identity.volume_id);
            }
        }
        VolumeCmd::Status { mount_point } => {
            let mount = std::path::Path::new(mount_point);
            let (identity, stats) = trove.volume_status(mount)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "identity": identity,
                        "stats": stats,
                    }))?
                );
            } else {
                println!(
                    "volume {} (label: {})  copied={} pending={} stale={} failed={}",
                    identity.volume_id,
                    identity.label.as_deref().unwrap_or("-"),
                    stats.copied,
                    stats.pending,
                    stats.stale,
                    stats.failed,
                );
            }
        }
        VolumeCmd::List => {
            let volumes = trove.volume_list()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&volumes)?);
            } else if volumes.is_empty() {
                println!("no volumes");
            } else {
                for v in volumes {
                    println!("{}  {}", v.volume_id, v.label.as_deref().unwrap_or("-"));
                }
            }
        }
        VolumeCmd::Diff {
            mount_point,
            playlist,
            query,
        } => {
            let mount = std::path::Path::new(mount_point);
            let spec = query_to_spec(query)?;
            let diff = trove.volume_diff(
                mount,
                playlist.as_deref(),
                if playlist.is_some() {
                    None
                } else {
                    Some(&spec)
                },
                cli.offline,
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&diff)?);
            } else {
                for entry in diff {
                    println!(
                        "{:>7}  {}  {}",
                        format!("{:?}", entry.state).to_lowercase(),
                        entry.track_id,
                        entry.relative_path
                    );
                }
            }
        }
    }
    Ok(())
}

fn sync(cli: &Cli, cmd: &SyncCmd) -> Result<()> {
    let mut trove = runtime::open_trove()?;
    match cmd {
        SyncCmd::Playlist { name, to, plan } => {
            let mount = std::path::Path::new(to);
            if *plan {
                let plan = trove.plan_playlist_sync(name, mount, cli.offline)?;
                print_sync_plan(name, to, &plan, cli.json);
            } else {
                let (plan, done, failed) = trove.run_playlist_sync(name, mount, cli.offline)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "plan": plan,
                            "transferred": done,
                            "failed": failed,
                        }))?
                    );
                } else {
                    println!(
                        "synced '{name}' -> {to}: {done} transferred, {failed} failed, {} skipped",
                        plan.already_present.len()
                    );
                }
            }
        }
        SyncCmd::Query { to, plan, filters } => {
            let mount = std::path::Path::new(to);
            let spec = query_to_spec(filters)?;
            let sync_plan = trove.plan_query_sync(&spec, mount, cli.offline)?;
            if *plan {
                print_sync_plan("query", to, &sync_plan, cli.json);
            } else {
                anyhow::bail!("query sync execution is not wired yet; use --plan to preview");
            }
        }
        SyncCmd::Resume => {
            let (done, failed) = trove.resume_sync(cli.offline)?;
            println!("resumed sync: {done} transferred, {failed} failed");
        }
        SyncCmd::Verify {
            mount_point,
            playlist,
            query,
        } => {
            let mount = std::path::Path::new(mount_point);
            let spec = query_to_spec(query)?;
            let (present, missing, stale) = trove.verify_volume_sync(
                mount,
                playlist.as_deref(),
                if playlist.is_some() {
                    None
                } else {
                    Some(&spec)
                },
                cli.offline,
            )?;
            println!("verify {mount_point}: {present} present, {missing} missing, {stale} stale");
        }
    }
    Ok(())
}

fn print_sync_plan(name: &str, mount: &str, plan: &trove_core::sync::SyncPlan, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "playlist": name,
                "mount": mount,
                "plan": plan,
            }))
            .expect("serialize plan")
        );
        return;
    }
    println!(
        "sync plan for '{name}' -> {mount}: {} transfer(s), {} byte(s), {} already present",
        plan.transfers.len(),
        plan.bytes_remaining,
        plan.already_present.len()
    );
    for t in &plan.transfers {
        println!("  {} -> {}", t.object_key, t.relative_path);
    }
}

fn query_to_spec(args: &QueryArgs) -> Result<QuerySpec> {
    let (bpm_min, bpm_max) = parse_range(args.bpm.as_deref())?;
    Ok(QuerySpec {
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
    })
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
