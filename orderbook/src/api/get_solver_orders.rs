use crate::api::convert_get_orders_error_to_reply;
use crate::orderbook::Orderbook;
use anyhow::Result;
use model::order::SolverOrder;
use std::{convert::Infallible, sync::Arc};
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

fn get_solver_orders_request() -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::path!("solver_orders").and(warp::get())
}

fn get_solver_orders_response(result: Result<Vec<SolverOrder>>) -> impl Reply {
    match result {
        Ok(orders) => Ok(reply::with_status(reply::json(&orders), StatusCode::OK)),
        Err(err) => Ok(convert_get_orders_error_to_reply(err)),
    }
}

pub fn get_solver_orders(
    orderbook: Arc<Orderbook>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    get_solver_orders_request().and_then(move || {
        let orderbook = orderbook.clone();
        async move {
            let result = orderbook.get_solvable_orders().await;
            Result::<_, Infallible>::Ok(get_solver_orders_response(result))
        }
    })
}
