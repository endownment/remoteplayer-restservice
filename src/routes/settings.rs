use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::ApiError;
use crate::lifecycle::{ApplyReport, apply_settings};
use crate::routes::SharedContext;
use crate::routes::extract::JsonBody;
use crate::settings::{ApplyMode, PlayerSettings, SettingsPatch};

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SettingsResponse {
    settings: PlayerSettings,
    defaults: PlayerSettings,
    apply_modes: BTreeMap<&'static str, ApplyMode>,
}

pub async fn get_settings(State(context): State<SharedContext>) -> Json<SettingsResponse> {
    Json(SettingsResponse {
        settings: context.settings.current(),
        defaults: context.settings.defaults().clone(),
        apply_modes: PlayerSettings::apply_modes().into_iter().collect(),
    })
}

/// Full replacement: every key missing from the body gets its default value.
pub async fn put_settings(
    State(context): State<SharedContext>,
    JsonBody(patch): JsonBody<SettingsPatch>,
) -> Result<Json<ApplyReport>, ApiError> {
    let defaults: &PlayerSettings = context.settings.defaults();
    let next: PlayerSettings = defaults.with_patch(&patch, defaults);
    Ok(Json(apply_settings(&context, next).await?))
}

/// JSON Merge Patch: missing keys stay as they are, `null` resets to the default.
pub async fn patch_settings(
    State(context): State<SharedContext>,
    JsonBody(patch): JsonBody<SettingsPatch>,
) -> Result<Json<ApplyReport>, ApiError> {
    let next: PlayerSettings = context.settings.current().with_patch(&patch, context.settings.defaults());
    Ok(Json(apply_settings(&context, next).await?))
}
