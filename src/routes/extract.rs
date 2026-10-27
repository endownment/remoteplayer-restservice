use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// `axum::Json`, except that a malformed body is answered with Problem Details
/// JSON like every other error, instead of axum's plain-text rejection.
pub struct JsonBody<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for JsonBody<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<JsonBody<T>, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(JsonBody(value)),
            Err(rejection) => Err(ApiError::new(rejection.status(), rejection.body_text())),
        }
    }
}

/// The same for query strings.
pub struct QueryParams<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for QueryParams<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<QueryParams<T>, ApiError> {
        let extracted: Result<Query<T>, QueryRejection> = Query::<T>::from_request_parts(parts, state).await;
        match extracted {
            Ok(Query(value)) => Ok(QueryParams(value)),
            Err(rejection) => Err(ApiError::new(rejection.status(), rejection.body_text())),
        }
    }
}
