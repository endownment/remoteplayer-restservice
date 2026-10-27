use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app_context::AppContext;
use crate::error::ApiError;

const TOKEN_QUERY_PARAMETER: &str = "access_token";

/// Accepts `Authorization: Bearer <token>`, or `?access_token=<token>` because
/// browsers cannot set headers on WebSocket connections.
pub async fn require_token(State(context): State<Arc<AppContext>>, request: Request, next: Next) -> Response {
    if context.config.auth.disabled {
        return next.run(request).await;
    }
    let expected: &str = context.config.auth.token.as_deref().unwrap_or_default();
    let presented: Option<String> = bearer_token(&request).or_else(|| query_token(&request));
    match presented {
        Some(token) if constant_time_equals(token.as_bytes(), expected.as_bytes()) => next.run(request).await,
        _ => ApiError::unauthorized().into_response(),
    }
}

fn bearer_token(request: &Request) -> Option<String> {
    let value: &str = request.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|token: &str| token.trim().to_string())
}

fn query_token(request: &Request) -> Option<String> {
    let query: &str = request.uri().query()?;
    query.split('&').find_map(|pair: &str| {
        let (key, value): (&str, &str) = pair.split_once('=')?;
        (key == TOKEN_QUERY_PARAMETER).then(|| value.to_string())
    })
}

/// Compares without an early exit, so response timing does not reveal how many
/// leading characters of a guessed token were right.
fn constant_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference: u8 = left.iter().zip(right).fold(0u8, |accumulator: u8, (a, b): (&u8, &u8)| accumulator | (a ^ b));
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison_matches_equality() {
        assert!(constant_time_equals(b"secret", b"secret"));
        assert!(!constant_time_equals(b"secret", b"secreT"));
        assert!(!constant_time_equals(b"secret", b"secret-longer"));
    }
}
