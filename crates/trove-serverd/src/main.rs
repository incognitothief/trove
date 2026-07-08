//! `trove-serverd` — a thin local HTTP/JSON API over `trove-core`.
//!
//! It exposes core operations for the React UI and holds no business logic of
//! its own: each handler locks the shared [`Trove`] and forwards to a core
//! method. The daemon is local-only by design (binds to loopback).

mod runtime;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use trove_core::model::TrackId;
use trove_core::query::QuerySpec;
use trove_core::Trove;

/// Shared, mutex-guarded core handle. Core calls are synchronous and never held
/// across an `.await`, so a std `Mutex` is sufficient.
type AppState = Arc<Mutex<Trove>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let trove = runtime::open_trove()?;
    let state: AppState = Arc::new(Mutex::new(trove));

    let app = Router::new()
        .route("/health", get(health))
        .route("/reconcile", post(reconcile))
        .route("/query", post(query))
        .route("/playlists", get(list_playlists).post(create_playlist))
        .route("/playlists/:name", get(get_playlist))
        .route("/playlists/:name/tracks", post(add_tracks))
        .route("/import", post(import))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = std::env::var("TROVE_SERVERD_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:7377".to_string())
        .parse()?;
    tracing::info!("trove-serverd listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "trove-serverd" }))
}

#[derive(Deserialize)]
struct ReconcileBody {
    #[serde(default)]
    offline: bool,
}

async fn reconcile(
    State(state): State<AppState>,
    Json(body): Json<ReconcileBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut trove = lock(&state)?;
    let report = trove.reconcile(body.offline)?;
    Ok(Json(json!({ "report": format!("{report:?}") })))
}

#[derive(Deserialize)]
struct QueryBody {
    #[serde(default)]
    offline: bool,
    #[serde(flatten)]
    spec: QuerySpec,
}

async fn query(
    State(state): State<AppState>,
    Json(body): Json<QueryBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut trove = lock(&state)?;
    let results = trove.query(&body.spec, body.offline)?;
    Ok(Json(json!({ "tracks": results })))
}

async fn list_playlists(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let trove = lock(&state)?;
    Ok(Json(json!({ "playlists": trove.playlist_list()? })))
}

#[derive(Deserialize)]
struct CreatePlaylistBody {
    name: String,
}

async fn create_playlist(
    State(state): State<AppState>,
    Json(body): Json<CreatePlaylistBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let trove = lock(&state)?;
    let playlist = trove.playlist_create(&body.name)?;
    Ok(Json(json!({ "playlist": playlist })))
}

async fn get_playlist(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let trove = lock(&state)?;
    Ok(Json(json!({ "playlist": trove.playlist_get(&name)? })))
}

#[derive(Deserialize)]
struct AddTracksBody {
    track_ids: Vec<String>,
}

async fn add_tracks(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<AddTracksBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let trove = lock(&state)?;
    let ids: Vec<TrackId> = body.track_ids.into_iter().map(TrackId).collect();
    trove.playlist_add(&name, &ids)?;
    Ok(Json(json!({ "added": ids.len() })))
}

#[derive(Deserialize)]
struct ImportBody {
    path: String,
    #[serde(default)]
    plan_only: bool,
}

async fn import(
    State(state): State<AppState>,
    Json(body): Json<ImportBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut trove = lock(&state)?;
    let options = trove_core::ImportOptions {
        include_dotfiles: trove.config.import.include_dotfiles,
        capture_artwork: trove.config.import.capture_artwork,
    };
    let mut job = trove.import_plan(std::path::Path::new(&body.path), &options)?;
    let stats = job.stats();
    if body.plan_only {
        return Ok(Json(json!({
            "job_id": job.id,
            "total": stats.total,
            "duplicates": stats.duplicates,
            "artwork_candidates": job.artwork.len(),
        })));
    }
    let committed = trove.import_run(&mut job)?;
    Ok(Json(json!({
        "job_id": job.id,
        "committed": committed,
        "artwork_captured": job.artwork.len(),
    })))
}

fn lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, Trove>, AppError> {
    state
        .lock()
        .map_err(|_| AppError(anyhow::anyhow!("core lock poisoned")))
}

/// Wraps any error into a JSON 500 response.
struct AppError(anyhow::Error);

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        AppError(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": format!("{:#}", self.0) }));
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}
