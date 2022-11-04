use crate::{api::Context, ban_manager::BanInfo};
use axum::{extract::State, Json};
use hyper::StatusCode;

#[allow(clippy::unused_async)]
pub(crate) async fn livez() -> StatusCode {
    StatusCode::OK
}

#[allow(clippy::unused_async)]
pub(crate) async fn readyz(State(state): State<Context>) -> StatusCode {
    if state.ready_indicator.status() {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    }
}

#[allow(clippy::unused_async)]
pub(crate) async fn get_banned(State(state): State<Context>) -> Json<Vec<BanInfo>> {
    Json(state.ban_manager.temp_bans())
}
