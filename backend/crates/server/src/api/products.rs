use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::orders;
use crate::domain::products::{
    self, CatalogQuery, CreateProductInput, UpdateProductInput, UploadInput,
};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub async fn list_categories(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let categories = products::list_categories(&state.db).await?;
    Ok(Json(json!({ "categories": categories })))
}

#[derive(Debug, Deserialize)]
pub struct ListProductsQuery {
    pub cat: Option<String>,
    pub q: Option<String>,
    pub sort: Option<String>,
    pub cursor: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

pub async fn list_products(
    State(state): State<AppState>,
    Query(q): Query<ListProductsQuery>,
) -> AppResult<Json<Value>> {
    let items = products::list_public_products(
        &state.db,
        CatalogQuery {
            cat: q.cat,
            q: q.q,
            sort: q.sort,
            cursor: q.cursor,
            limit: q.limit.unwrap_or(20),
        },
    )
    .await?;
    let next_cursor = items.last().map(|p| p.created_at);
    Ok(Json(
        json!({ "products": items, "next_cursor": next_cursor }),
    ))
}

pub async fn get_product(
    State(state): State<AppState>,
    Path(id_or_slug): Path<String>,
) -> AppResult<Json<products::ProductDetail>> {
    let detail = products::get_public_product_detail(&state.db, &id_or_slug).await?;
    Ok(Json(detail))
}

pub async fn get_shop(
    State(state): State<AppState>,
    Path(id_or_slug): Path<String>,
) -> AppResult<Json<products::PublicShopDetail>> {
    let detail = products::get_public_shop(&state.db, &id_or_slug).await?;
    Ok(Json(detail))
}

pub async fn list_reviews(
    State(state): State<AppState>,
    Path(product_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let reviews = orders::list_product_reviews(&state.db, product_id).await?;
    Ok(Json(json!({ "reviews": reviews })))
}

#[derive(Debug, Deserialize)]
pub struct CreateReviewRequest {
    pub rating: i16,
    #[serde(default)]
    pub body: String,
}

pub async fn create_review(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(req): Json<CreateReviewRequest>,
) -> AppResult<(StatusCode, Json<orders::Review>)> {
    let review =
        orders::create_review(&state.db, user.id, product_id, req.rating, &req.body).await?;
    Ok((StatusCode::CREATED, Json(review)))
}

// ---------- Người bán ----------

pub async fn list_seller_products(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Value>> {
    let items = products::list_seller_products(&state.db, user.id).await?;
    Ok(Json(json!({ "products": items })))
}

pub async fn create_product(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateProductInput>,
) -> AppResult<(StatusCode, Json<products::Product>)> {
    let product = products::create_product(&state.db, user.id, req).await?;
    Ok((StatusCode::CREATED, Json(product)))
}

pub async fn update_product(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(req): Json<UpdateProductInput>,
) -> AppResult<Json<products::Product>> {
    let product = products::update_product(&state.db, user.id, product_id, req).await?;
    Ok(Json(product))
}

#[derive(Debug, Deserialize)]
pub struct CreateVersionRequest {
    pub version: String,
    #[serde(default)]
    pub changelog: String,
}

pub async fn create_version(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(req): Json<CreateVersionRequest>,
) -> AppResult<(StatusCode, Json<products::ProductVersion>)> {
    let version =
        products::create_version(&state.db, user.id, product_id, &req.version, &req.changelog)
            .await?;
    Ok((StatusCode::CREATED, Json(version)))
}

pub async fn upload_version_file(
    State(state): State<AppState>,
    user: AuthUser,
    Path(version_id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<(StatusCode, Json<products::UploadResult>)> {
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        AppError::bad_request("invalid_multipart", format!("Lỗi đọc multipart: {e}"))
    })? {
        if field.name() != Some("file") && field.file_name().is_none() {
            continue;
        }
        let filename = field
            .file_name()
            .map(ToString::to_string)
            .unwrap_or_else(|| "file".to_string());
        let mime = field
            .content_type()
            .map(ToString::to_string)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::bad_request("upload_read_failed", format!("Lỗi đọc tệp: {e}")))?
            .to_vec();
        let result = products::upload_version_file(
            &state.db,
            state.storage.as_ref(),
            state.scanner.as_ref(),
            UploadInput {
                version_id,
                owner_id: user.id,
                filename,
                mime,
                data,
            },
        )
        .await?;
        return Ok((StatusCode::CREATED, Json(result)));
    }
    Err(AppError::bad_request(
        "missing_file",
        "Thiếu trường tệp 'file' trong yêu cầu multipart.",
    ))
}

#[derive(Debug, Deserialize)]
pub struct SubmitVersionRequest {
    pub commitment_accepted: bool,
}

pub async fn submit_version(
    State(state): State<AppState>,
    user: AuthUser,
    Path(version_id): Path<Uuid>,
    Json(req): Json<SubmitVersionRequest>,
) -> AppResult<Json<products::ProductVersion>> {
    let ver =
        products::submit_version(&state.db, user.id, version_id, req.commitment_accepted).await?;
    Ok(Json(ver))
}
