use crate::{driver::solver_settlements::RatedSettlement, metrics::SolverMetrics, solver::Solver};
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

fn get_prices(rated_settlement: &RatedSettlement) -> HashMap<&H160, BigRational> {
    rated_settlement
        .settlement
        .clearing_prices()
        .iter()
        .map(|(token, price)| (token, price.to_big_rational()))
        .collect::<HashMap<_, _>>()
}

/// Record metric with surplus achieved in winning settlement
/// vs that which was unrealized in other feasible solutions.
pub fn report_alternative_settlement_surplus(
    metrics: Arc<dyn SolverMetrics>,
    winning_settlement: &(Arc<dyn Solver>, RatedSettlement),
    alternative_settlements: Vec<(Arc<dyn Solver>, RatedSettlement)>,
) {
    let (winning_solver, submitted) = winning_settlement;
    let submitted_prices = get_prices(submitted);
    let submitted_surplus: HashMap<_, _> = submitted
        .settlement
        .trades()
        .iter()
        .map(|trade| {
            let sell_token_price = &submitted_prices[&trade.order.order_creation.sell_token];
            let buy_token_price = &submitted_prices[&trade.order.order_creation.buy_token];
            (
                trade.order.order_meta_data.uid,
                SurplusInfo {
                    solver_name: winning_solver.name(),
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

    let mut best_alternative: HashMap<OrderUid, SurplusInfo> = HashMap::new();
    for (solver, settlement) in alternative_settlements.iter() {
        let trades = settlement.settlement.trades();
        let clearing_prices = get_prices(settlement);
        for trade in trades {
            let order_id = trade.order.order_meta_data.uid;
            let sell_token_price = &clearing_prices[&trade.order.order_creation.sell_token];
            let buy_token_price = &clearing_prices[&trade.order.order_creation.buy_token];
            if submitted_surplus.contains_key(&order_id) {
                let surplus = SurplusInfo {
                    solver_name: solver.name(),
                    absolute: trade
                        .surplus(sell_token_price, buy_token_price)
                        .unwrap_or_else(BigRational::zero),
                    ratio: trade
                        .surplus_ratio(sell_token_price, buy_token_price)
                        .unwrap_or_else(BigRational::zero),
                };
                let entry = best_alternative.entry(order_id);
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
    }

    for (order_id, submitted) in submitted_surplus.iter() {
        if let Some(alternative) = best_alternative.get(order_id) {
            metrics.report_order_surplus(
                winning_solver.name(),
                alternative.solver_name,
                (&submitted.ratio - &alternative.ratio)
                    .to_f64()
                    .unwrap_or_default(),
            );
            if alternative.absolute
                > &BigRational::new(101.into(), 100.into()) * &submitted.absolute
            {
                tracing::warn!("submission surplus worse than lower ranked settlement; order {:?} submitted {}, best alternative {}", order_id, submitted, alternative)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // use crate::settlement::{Settlement, SettlementEncoder, Trade};
    // use ethcontract::{H160, U256};
    // use maplit::hashmap;
    // use model::order::{Order, OrderCreation};

    #[test]
    fn num_unsettled_orders_with_better_surplus_() {
        // let buy_token = H160::from_low_u64_be(1);
        // let sell_token = H160::from_low_u64_be(2);
        // let trade = Trade {
        //     order: Order {
        //         order_meta_data: Default::default(),
        //         order_creation: OrderCreation {
        //             sell_token,
        //             buy_token,
        //             sell_amount: 10.into(),
        //             buy_amount: 5.into(),
        //             ..Default::default()
        //         },
        //     },
        //     executed_amount: 10.into(),
        //     ..Default::default()
        // };
        //
        // let submitted_clearing_prices = hashmap! {
        //     sell_token => U256::from(1000),
        //     buy_token => U256::from(1000),
        // };
        // let similar_clearing_prices = hashmap! {
        //     sell_token => U256::from(1000),
        //     buy_token => U256::from(990),
        // };
        // let better_clearing_prices = hashmap! {
        //     sell_token => U256::from(1000),
        //     buy_token => U256::from(989),
        // };
        //
        // let submitted_settlement = Settlement {
        //     encoder: SettlementEncoder::with_trades(submitted_clearing_prices, vec![trade.clone()]),
        // };
        // let better_settlement_1 = Settlement {
        //     encoder: SettlementEncoder::with_trades(
        //         better_clearing_prices.clone(),
        //         vec![trade.clone()],
        //     ),
        // };
        // let better_settlement_2 = Settlement {
        //     encoder: SettlementEncoder::with_trades(better_clearing_prices, vec![trade.clone()]),
        // };
        // let similar_settlement = Settlement {
        //     encoder: SettlementEncoder::with_trades(similar_clearing_prices, vec![trade]),
        // };
        // TODO - Make some assertions here.
    }
}
