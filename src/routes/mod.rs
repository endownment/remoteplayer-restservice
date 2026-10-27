mod events;
mod extract;
mod hardware;
mod library;
mod player;
mod settings;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::routing::{get, post, put};
use axum::{Json, Router, middleware};
use serde::Serialize;
use tower_http::trace::TraceLayer;

use crate::app_context::AppContext;
use crate::auth;

/// Handlers take `State<Arc<AppContext>>`; axum clones the Arc (a counter bump)
/// for every request.
pub type SharedContext = Arc<AppContext>;

pub fn build_router(context: SharedContext) -> Router {
    // axum limits request bodies to 2 MB by default; uploads stream to disk and
    // enforce their own limit (storage.max-upload-bytes) instead.
    let uploads: Router<SharedContext> = Router::new()
        .route("/api/v1/folders/{folder}/files/{name}", put(library::put_file).delete(library::delete_file))
        .route("/api/v1/uploads", post(library::post_uploads))
        .layer(DefaultBodyLimit::disable());

    let protected: Router<SharedContext> = Router::new()
        .route(
            "/api/v1/settings",
            get(settings::get_settings).put(settings::put_settings).patch(settings::patch_settings),
        )
        .route("/api/v1/audio/devices", get(hardware::audio_devices))
        .route("/api/v1/display", get(hardware::display))
        .route("/api/v1/cec", get(hardware::cec))
        .route("/api/v1/system", get(hardware::system))
        .route("/api/v1/system/reboot", post(hardware::reboot))
        .route("/api/v1/system/poweroff", post(hardware::power_off))
        .route("/api/v1/storage", get(library::storage))
        .route("/api/v1/folders", get(library::list_folders).post(library::create_folder))
        .route("/api/v1/folders/{folder}", axum::routing::delete(library::delete_folder))
        .route("/api/v1/folders/{folder}/files", get(library::list_files))
        .route("/api/v1/folders/{folder}/files/{name}/md5", get(library::file_md5))
        .route("/api/v1/folders/{folder}/files/{name}/move", post(library::move_file))
        .route("/api/v1/player/state", get(player::state))
        .route("/api/v1/player/command", post(player::command))
        .route("/api/v1/player/ipc", post(player::raw_ipc))
        .route("/api/v1/player/restart", post(player::restart))
        .route("/api/v1/player/restore", post(player::restore))
        .route("/api/v1/player/service", get(player::service))
        .route("/api/v1/player/log", get(player::log))
        .route("/api/v1/player/events", get(events::events))
        .merge(uploads)
        .route_layer(middleware::from_fn_with_state(Arc::clone(&context), auth::require_token));

    Router::new()
        .route("/api/v1/health", get(health))
        .merge(protected)
        .layer(TraceLayer::new_for_http())
        .with_state(context)
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    mpv_connected: bool,
}

async fn health(State(context): State<SharedContext>) -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok", version: env!("CARGO_PKG_VERSION"), mpv_connected: context.mpv.is_connected() })
}
