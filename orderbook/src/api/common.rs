use anyhow::Error;
use warp::{hyper::StatusCode, reply};

pub fn convert_get_orders_error_to_reply(err: Error) -> reply::WithStatus<reply::Json> {
    tracing::error!(?err, "get_orders error");
    return reply::with_status(super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR);
}
