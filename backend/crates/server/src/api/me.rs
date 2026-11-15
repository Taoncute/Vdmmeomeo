use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::UserDto;
use crate::auth::{load_auth_user, AuthUser};
use crate::domain::stall::{self, Shop};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct MeResponse {
    #[serde(flatten)]
    pub user: UserDto,
    /// Gian hàng của người dùng (nếu đã từng thuê).
    pub shop: Option<Shop>,
}

pub async fn get_me(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<MeResponse>> {
    let shop = stall::shop_of_owner(&state.db, user.id).await?;
    Ok(Json(MeResponse {
        user: UserDto::from(&user),
        shop,
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateMeRequest {
    pub display_name: Option<String>,
    /// Chuỗi rỗng nghĩa là xóa ảnh đại diện.
    pub avatar_url: Option<String>,
}

pub async fn patch_me(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpdateMeRequest>,
) -> AppResult<Json<UserDto>> {
    let mut display_name = user.display_name.clone();
    let mut avatar_url = user.avatar_url.clone();

    if let Some(name) = req.display_name {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > 60 {
            return Err(AppError::bad_request(
                "validation_error",
                "Tên hiển thị phải dài từ 1 đến 60 ký tự.",
            ));
        }
        display_name = name;
    }
    if let Some(url) = req.avatar_url {
        let url = url.trim().to_string();
        if url.is_empty() {
            avatar_url = None;
        } else if !url.starts_with("https://") || url.len() > 500 {
            return Err(AppError::bad_request(
                "validation_error",
                "Ảnh đại diện phải là đường dẫn https dài tối đa 500 ký tự.",
            ));
        } else {
            avatar_url = Some(url);
        }
    }

    sqlx::query("UPDATE users SET display_name = $2, avatar_url = $3 WHERE id = $1")
        .bind(user.id)
        .bind(display_name.as_str())
        .bind(avatar_url.as_deref())
        .execute(&state.db)
        .await?;
    let updated = load_auth_user(&state.db, user.id)
        .await?
        .ok_or_else(|| AppError::not_found("Tài khoản không tồn tại."))?;
    Ok(Json(UserDto::from(&updated)))
}
