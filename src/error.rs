//! Every failure leaves the API as RFC 9457 "Problem Details" JSON.
//! Domain errors are converted here, at the HTTP boundary, so the modules
//! below stay free of HTTP concepts.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::audio::AudioError;
use crate::cec::CecError;
use crate::fs_util::{describe_io_error, is_write_refusal};
use crate::lifecycle::LifecycleError;
use crate::mpv::{MpvError, RawCommandError};
use crate::player_command::CommandError;
use crate::power::PowerError;
use crate::service_manager::ServiceError;
use crate::settings::SettingsError;
use crate::storage::StorageError;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    detail: String,
    log: Vec<String>,
}

#[derive(Serialize)]
struct ProblemDetails {
    #[serde(rename = "type")]
    problem_type: &'static str,
    title: String,
    status: u16,
    detail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    log: Vec<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> ApiError {
        ApiError { status, detail: detail.into(), log: Vec::new() }
    }

    pub fn bad_request(detail: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, detail)
    }

    pub fn unauthorized() -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "missing or wrong bearer token")
    }

    pub fn forbidden(detail: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, detail)
    }

    fn with_log(mut self, log: &[String]) -> ApiError {
        self.log = log.to_vec();
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::warn!(status = %self.status, detail = %self.detail, "request failed");
        }
        let body: ProblemDetails = ProblemDetails {
            problem_type: "about:blank",
            title: self.status.canonical_reason().unwrap_or("Error").to_string(),
            status: self.status.as_u16(),
            detail: self.detail,
            log: self.log,
        };
        let mut response: Response = (self.status, Json(body)).into_response();
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/problem+json"));
        response
    }
}

// `?` in a handler calls `From::from` on the error, so each impl below decides
// the HTTP status for one domain error type.

impl From<MpvError> for ApiError {
    fn from(error: MpvError) -> ApiError {
        let status: StatusCode = match error {
            MpvError::NotConnected | MpvError::ConnectionLost => StatusCode::SERVICE_UNAVAILABLE,
            MpvError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            MpvError::Rejected(_) | MpvError::UnexpectedReply(_) => StatusCode::BAD_GATEWAY,
        };
        ApiError::new(status, error.to_string())
    }
}

impl From<StorageError> for ApiError {
    fn from(error: StorageError) -> ApiError {
        let status: StatusCode = match error {
            StorageError::InvalidName(_)
            | StorageError::InvalidChecksum(_)
            | StorageError::ChecksumMismatch { .. }
            | StorageError::Interrupted(_) => StatusCode::BAD_REQUEST,
            StorageError::FolderNotFound(_) | StorageError::FileNotFound(_) => StatusCode::NOT_FOUND,
            StorageError::AlreadyExists(_) | StorageError::FolderNotEmpty(_) => StatusCode::CONFLICT,
            StorageError::OutsideRoot(_)
            | StorageError::ReadOnly(_)
            | StorageError::PermissionDenied(_)
            | StorageError::Volatile(_) => StatusCode::FORBIDDEN,
            StorageError::NotConfigured => StatusCode::NOT_IMPLEMENTED,
            StorageError::RootUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            StorageError::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            StorageError::InsufficientSpace { .. } | StorageError::DiskFull => StatusCode::INSUFFICIENT_STORAGE,
            StorageError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError::new(status, error.to_string())
    }
}

impl From<SettingsError> for ApiError {
    fn from(error: SettingsError) -> ApiError {
        settings_status_and_detail(&error).into()
    }
}

/// Shared by `SettingsError` and `LifecycleError::Settings`.
fn settings_status_and_detail(error: &SettingsError) -> (StatusCode, String) {
    match error {
        SettingsError::Invalid(_) => (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
        SettingsError::Io { path, source } if is_write_refusal(source) => (
            StatusCode::FORBIDDEN,
            format!("settings cannot be saved, {}: {}; nothing was changed", path.display(), describe_io_error(source)),
        ),
        SettingsError::Io { .. } | SettingsError::Encode(_) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

impl From<(StatusCode, String)> for ApiError {
    fn from((status, detail): (StatusCode, String)) -> ApiError {
        ApiError::new(status, detail)
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> ApiError {
        ApiError::new(StatusCode::BAD_GATEWAY, error.to_string())
    }
}

impl From<LifecycleError> for ApiError {
    fn from(error: LifecycleError) -> ApiError {
        let log: Vec<String> = error.log().to_vec();
        let status: StatusCode = match &error {
            LifecycleError::Busy => StatusCode::CONFLICT,
            LifecycleError::LiveChangeRejected { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            LifecycleError::NoWatcher => StatusCode::NOT_IMPLEMENTED,
            LifecycleError::NothingToRestore => StatusCode::NOT_FOUND,
            LifecycleError::Watcher(_) => StatusCode::BAD_GATEWAY,
            LifecycleError::MpvDidNotStart { .. }
            | LifecycleError::VideoOutputFailed { .. }
            | LifecycleError::RolledBack { .. }
            | LifecycleError::Service(_) => StatusCode::BAD_GATEWAY,
            LifecycleError::Settings(settings) => return ApiError::from(settings_status_and_detail(settings)),
        };
        ApiError::new(status, error.to_string()).with_log(&log)
    }
}

impl From<CommandError> for ApiError {
    fn from(error: CommandError) -> ApiError {
        match error {
            CommandError::Storage(storage) => ApiError::from(storage),
            CommandError::Invalid(detail) => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, detail),
        }
    }
}

impl From<RawCommandError> for ApiError {
    fn from(error: RawCommandError) -> ApiError {
        let status: StatusCode = match error {
            RawCommandError::Malformed => StatusCode::BAD_REQUEST,
            _ => StatusCode::FORBIDDEN,
        };
        ApiError::new(status, error.to_string())
    }
}

impl From<CecError> for ApiError {
    fn from(error: CecError) -> ApiError {
        let status: StatusCode = match error {
            CecError::Disabled => StatusCode::NOT_IMPLEMENTED,
            CecError::Open { .. } => StatusCode::SERVICE_UNAVAILABLE,
            CecError::Ioctl { .. } | CecError::Worker(_) => StatusCode::BAD_GATEWAY,
        };
        ApiError::new(status, error.to_string())
    }
}

impl From<PowerError> for ApiError {
    fn from(error: PowerError) -> ApiError {
        let status: StatusCode = match error {
            PowerError::Disabled => StatusCode::NOT_IMPLEMENTED,
            PowerError::DBus(_) => StatusCode::BAD_GATEWAY,
        };
        ApiError::new(status, error.to_string())
    }
}

impl From<AudioError> for ApiError {
    fn from(error: AudioError) -> ApiError {
        match error {
            AudioError::Mpv(mpv) => ApiError::from(mpv),
            AudioError::Spawn { .. } | AudioError::Timeout(_) => {
                ApiError::new(StatusCode::BAD_GATEWAY, error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn settings_on_a_read_only_filesystem_are_forbidden_not_internal_errors() {
        let error: SettingsError = SettingsError::Io {
            path: PathBuf::from("/srv/remoteplayer/state/restservice/mpv.env"),
            source: std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem),
        };
        let api_error: ApiError = ApiError::from(LifecycleError::Settings(error));
        assert_eq!(api_error.status, StatusCode::FORBIDDEN);
        assert!(api_error.detail.contains("nothing was changed"));
    }

    #[test]
    fn library_without_upload_root_is_not_implemented() {
        assert_eq!(ApiError::from(StorageError::NotConfigured).status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            ApiError::from(StorageError::RootUnavailable("/srv".to_string())).status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
