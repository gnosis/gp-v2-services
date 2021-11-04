use crate::{metrics::SolverMetrics, settlement::Settlement};
use bigdecimal::{ToPrimitive, Zero};
use ethcontract::H160;
use model::order::OrderUid;
use num::BigRational;
use shared::conversions::U256Ext;
use std::{
    collections::{hash_map::Entry, HashMap},
    fmt::{Display, Formatter},
    sync::Arc,
};

#[derive(Clone)]
struct SurplusInfo {
    solver_name: &'static str,
    absolute: BigRational,
    ratio: BigRational,
}

impl Display for SurplusInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Surplus {{solver: {}, absolute: {:.2e}, ratio: {:.2e} }}",
            self.solver_name,
            self.absolute.to_f64().unwrap_or(f64::NAN),
            self.ratio.to_f64().unwrap_or(f64::NAN)
        )
    }
}

fn get_prices(settlement: &Settlement) -> HashMap<&H160, BigRational> {
    settlement
        .clearing_prices()
        .iter()
        .map(|(token, price)| (token, price.to_big_rational()))
        .collect::<HashMap<_, _>>()
}

/// Record metric with surplus achieved in winning settlement
/// vs that which was unrealized in other feasible solutions.
pub fn report_alternative_settlement_surplus(
    metrics: Arc<dyn SolverMetrics>,
    winning_settlement: (&'static str, &Settlement),
    alternative_settlements: Vec<(&'static str, &Settlement)>,
) {
    let (winning_solver, submitted) = winning_settlement;
    let submitted_prices = get_prices(submitted);
    let submitted_surplus: HashMap<_, _> = submitted
        .trades()
        .iter()
        .map(|trade| {
            let sell_token_price = &submitted_prices[&trade.order.order_creation.sell_token];
            let buy_token_price = &submitted_prices[&trade.order.order_creation.buy_token];
            (
                trade.order.order_meta_data.uid,
                SurplusInfo {
                    solver_name: winning_solver,
                    absolute: trade
                        .surplus(sell_token_price, buy_token_price)
                        .unwrap_or_else(BigRational::zero),
                    ratio: trade
                        .surplus_ratio(sell_token_price, buy_token_price)
                        .unwrap_or_else(BigRational::zero),
                },
            )
        })
        .collect();

    let best_alternative = best_surplus_by_order(alternative_settlements);

    for (order_id, submitted) in submitted_surplus.iter() {
        if let Some(alternative) = best_alternative.get(order_id) {
            metrics.report_order_surplus(
                winning_solver,
                alternative.solver_name,
                (&submitted.ratio - &alternative.ratio)
                    .to_f64()
                    .unwrap_or_default(),
            );
            if alternative.absolute > submitted.absolute {
                tracing::warn!("submission surplus worse than lower ranked settlement; order {:?} submitted {}, best alternative {}", order_id, submitted, alternative)
            }
        }
    }
}

fn best_surplus_by_order(
    settlements: Vec<(&'static str, &Settlement)>,
) -> HashMap<OrderUid, SurplusInfo> {
    let mut best_surplus: HashMap<OrderUid, SurplusInfo> = HashMap::new();
    for (solver, settlement) in settlements.iter() {
        let trades = settlement.trades();
        let clearing_prices = get_prices(settlement);
        for trade in trades {
            let order_id = trade.order.order_meta_data.uid;
            let sell_token_price = &clearing_prices[&trade.order.order_creation.sell_token];
            let buy_token_price = &clearing_prices[&trade.order.order_creation.buy_token];
            let surplus = SurplusInfo {
                solver_name: solver,
                absolute: trade
                    .surplus(sell_token_price, buy_token_price)
                    .unwrap_or_else(BigRational::zero),
                ratio: trade
                    .surplus_ratio(sell_token_price, buy_token_price)
                    .unwrap_or_else(BigRational::zero),
            };
            let entry = best_surplus.entry(order_id);
            match entry {
                Entry::Occupied(mut entry) => {
                    let value = entry.get_mut();
                    if value.absolute < surplus.absolute {
                        *value = surplus;
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(surplus);
                }
            }
        }
    }
    best_surplus
}
