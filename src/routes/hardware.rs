use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::audio::AudioDeviceList;
use crate::cec::CecReport;
use crate::display::{self, DisplayInfo};
use crate::error::ApiError;
use crate::power::PowerAction;
use crate::routes::SharedContext;
use crate::system_info::{self, SystemInfo};

pub async fn audio_devices(State(context): State<SharedContext>) -> Result<Json<AudioDeviceList>, ApiError> {
    let configured: String = context.settings.current().audio_device;
    Ok(Json(context.audio.list(&context.mpv, &configured).await?))
}

pub async fn display(State(context): State<SharedContext>) -> Result<Json<DisplayInfo>, ApiError> {
    let preferred: Option<&str> = context.config.display.preferred_connector.as_deref();
    Ok(Json(display::describe(&context.config.display.drm_sysfs_root, preferred, &context.mpv).await?))
}

pub async fn cec(State(context): State<SharedContext>) -> Result<Json<CecReport>, ApiError> {
    Ok(Json(context.cec.scan().await?))
}

pub async fn system(State(context): State<SharedContext>) -> Json<SystemInfo> {
    Json(system_info::collect(&context.config.system).await)
}

pub async fn reboot(State(context): State<SharedContext>) -> Result<StatusCode, ApiError> {
    context.power.perform(PowerAction::Reboot).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn power_off(State(context): State<SharedContext>) -> Result<StatusCode, ApiError> {
    context.power.perform(PowerAction::PowerOff).await?;
    Ok(StatusCode::ACCEPTED)
}
