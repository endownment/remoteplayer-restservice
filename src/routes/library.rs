use std::pin::Pin;

use axum::Json;
use axum::body::{Body, BodyDataStream};
use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;

use crate::error::ApiError;
use crate::routes::SharedContext;
use crate::routes::extract::{JsonBody, QueryParams};
use crate::storage::{EntryName, FileInfo, FolderInfo, Md5Info, StorageUsage, UploadRequest, UploadResult};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PutFileQuery {
    #[serde(default)]
    overwrite: bool,
    md5: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MultipartUploadQuery {
    folder: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CreateFolderRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MoveFileRequest {
    folder: String,
    name: Option<String>,
    #[serde(default)]
    overwrite: bool,
}

pub async fn storage(State(context): State<SharedContext>) -> Result<Json<StorageUsage>, ApiError> {
    Ok(Json(context.library.usage()?))
}

pub async fn list_folders(State(context): State<SharedContext>) -> Result<Json<Vec<FolderInfo>>, ApiError> {
    Ok(Json(context.library.list_folders().await?))
}

pub async fn create_folder(
    State(context): State<SharedContext>,
    JsonBody(request): JsonBody<CreateFolderRequest>,
) -> Result<StatusCode, ApiError> {
    let name: EntryName = parse_name(&request.name)?;
    context.library.create_folder(&name).await?;
    Ok(StatusCode::CREATED)
}

pub async fn delete_folder(
    State(context): State<SharedContext>,
    Path(folder): Path<String>,
) -> Result<StatusCode, ApiError> {
    context.library.delete_folder(&parse_name(&folder)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_files(
    State(context): State<SharedContext>,
    Path(folder): Path<String>,
) -> Result<Json<Vec<FileInfo>>, ApiError> {
    Ok(Json(context.library.list_files(&parse_name(&folder)?).await?))
}

pub async fn delete_file(
    State(context): State<SharedContext>,
    Path((folder, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    context.library.delete_file(&parse_name(&folder)?, &parse_name(&name)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn file_md5(
    State(context): State<SharedContext>,
    Path((folder, name)): Path<(String, String)>,
) -> Result<Json<Md5Info>, ApiError> {
    Ok(Json(context.library.file_md5(&parse_name(&folder)?, &parse_name(&name)?).await?))
}

pub async fn move_file(
    State(context): State<SharedContext>,
    Path((folder, name)): Path<(String, String)>,
    JsonBody(request): JsonBody<MoveFileRequest>,
) -> Result<Json<FileInfo>, ApiError> {
    let source_folder: EntryName = parse_name(&folder)?;
    let source_name: EntryName = parse_name(&name)?;
    let target_folder: EntryName = parse_name(&request.folder)?;
    let target_name: EntryName = match &request.name {
        Some(new_name) => parse_name(new_name)?,
        None => source_name.clone(),
    };
    let moved: FileInfo = context
        .library
        .move_file(&source_folder, &source_name, &target_folder, &target_name, request.overwrite)
        .await?;
    Ok(Json(moved))
}

/// `PUT /folders/{folder}/files/{name}` with the raw file as the body — the
/// simplest way for `curl -T`, scripts and apps. The folder is created if needed.
pub async fn put_file(
    State(context): State<SharedContext>,
    Path((folder, name)): Path<(String, String)>,
    QueryParams(query): QueryParams<PutFileQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<UploadResult>), ApiError> {
    let request: UploadRequest = UploadRequest {
        folder: parse_name(&folder)?,
        name: parse_name(&name)?,
        overwrite: query.overwrite,
        expected_md5: query.md5,
        content_length: content_length(&headers),
    };
    let stream: BodyDataStream = body.into_data_stream();
    let result: UploadResult = context.library.receive_upload(request, stream).await?;
    Ok((StatusCode::CREATED, Json(result)))
}

/// `POST /uploads?folder=...` with `multipart/form-data`, for HTML forms. Every
/// part that carries a file name is stored; without `folder` the configured
/// default folder is used.
pub async fn post_uploads(
    State(context): State<SharedContext>,
    QueryParams(query): QueryParams<MultipartUploadQuery>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Vec<UploadResult>>), ApiError> {
    let folder: EntryName = match &query.folder {
        Some(raw) => parse_name(raw)?,
        None => context.library.default_folder().clone(),
    };
    let mut results: Vec<UploadResult> = Vec::new();
    loop {
        let next: Option<Field<'_>> = multipart
            .next_field()
            .await
            .map_err(|error: axum::extract::multipart::MultipartError| ApiError::bad_request(error.to_string()))?;
        let Some(field): Option<Field<'_>> = next else { break };
        let Some(file_name): Option<String> = field.file_name().map(str::to_string) else { continue };
        let request: UploadRequest = UploadRequest {
            folder: folder.clone(),
            name: parse_name(&file_name)?,
            overwrite: query.overwrite,
            expected_md5: None,
            content_length: None,
        };
        // `Pin<Box<...>>` satisfies the `Unpin` bound of `receive_upload`
        // regardless of whether `Field` itself is Unpin.
        let stream: Pin<Box<Field<'_>>> = Box::pin(field);
        results.push(context.library.receive_upload(request, stream).await?);
    }
    if results.is_empty() {
        return Err(ApiError::bad_request("the form contained no file parts"));
    }
    Ok((StatusCode::CREATED, Json(results)))
}

fn parse_name(raw: &str) -> Result<EntryName, ApiError> {
    EntryName::parse(raw).map_err(|error: crate::storage::NameError| ApiError::bad_request(format!("`{raw}`: {error}")))
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers.get(header::CONTENT_LENGTH)?.to_str().ok()?.parse::<u64>().ok()
}
