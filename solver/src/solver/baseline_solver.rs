use crate::{
    liquidity::{
        AmmOrderExecution, ConstantProductOrder, LimitOrder, Liquidity, WeightedProductOrder,
    },
    settlement::Settlement,
    solver::Solver,
};
use anyhow::Result;
use ethcontract::{H160, U256};
use maplit::hashmap;
use model::TokenPair;
use num::BigRational;
use shared::{
    balancer::swap::{fixed_point::Bfp, WeightedPoolRef},
    baseline_solver::{
        estimate_buy_amount, estimate_sell_amount, path_candidates, BaselineSolvable,
        DEFAULT_MAX_HOPS,
    },
    sources::uniswap::pool_fetching::Pool,
};
use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom as _,
};

pub struct BaselineSolver {
    base_tokens: HashSet<H160>,
}

#[async_trait::async_trait]
impl Solver for BaselineSolver {
    async fn solve(&self, liquidity: Vec<Liquidity>, _gas_price: f64) -> Result<Vec<Settlement>> {
        Ok(self.solve(liquidity))
    }

    fn name(&self) -> &'static str {
        "BaselineSolver"
    }
}

impl BaselineSolvable for ConstantProductOrder {
    fn get_amount_out(&self, out_token: H160, in_amount: U256, in_token: H160) -> Option<U256> {
        amm_to_pool(self).get_amount_out(out_token, in_amount, in_token)
    }

    fn get_amount_in(&self, in_token: H160, out_amount: U256, out_token: H160) -> Option<U256> {
        amm_to_pool(self).get_amount_in(in_token, out_amount, out_token)
    }

    fn get_spot_price(&self, base_token: H160, quote_token: H160) -> Option<BigRational> {
        amm_to_pool(self).get_spot_price(base_token, quote_token)
    }

    fn gas_cost(&self) -> usize {
        amm_to_pool(self).gas_cost()
    }
}

impl BaselineSolvable for WeightedProductOrder {
    fn get_amount_out(&self, out_token: H160, in_amount: U256, in_token: H160) -> Option<U256> {
        amm_to_weighted_pool(self)
            .ok()?
            .get_amount_out(out_token, in_amount, in_token)
    }

    fn get_amount_in(&self, in_token: H160, out_amount: U256, out_token: H160) -> Option<U256> {
        amm_to_weighted_pool(self)
            .ok()?
            .get_amount_in(in_token, out_amount, out_token)
    }

    fn get_spot_price(&self, base_token: H160, quote_token: H160) -> Option<BigRational> {
        amm_to_weighted_pool(self)
            .ok()?
            .get_spot_price(base_token, quote_token)
    }

    fn gas_cost(&self) -> usize {
        amm_to_weighted_pool(self)
            .map(|pool| pool.gas_cost())
            .unwrap_or_default()
    }
}

impl BaselineSolver {
    pub fn new(base_tokens: HashSet<H160>) -> Self {
        Self { base_tokens }
    }

    fn solve(&self, liquidity: Vec<Liquidity>) -> Vec<Settlement> {
        let mut amm_map: HashMap<_, Vec<_>> = HashMap::new();
        for liquidity in &liquidity {
            if let Liquidity::ConstantProduct(cpo) = liquidity {
                let entry = amm_map.entry(cpo.tokens).or_default();
                entry.push(cpo.clone())
            }
        }

        // We assume that individual settlements do not move the amm pools significantly when
        // returning multiple settlements.
        let mut settlements = Vec::new();

        // Return a solution for the first settle-able user order
        for liquidity in liquidity {
            let user_order = match liquidity {
                Liquidity::Limit(order) => order,
                Liquidity::ConstantProduct(_) | Liquidity::WeightedProduct(_) => continue,
            };

            let solution = match self.settle_order(&user_order, &amm_map) {
                Some(solution) => solution,
                None => continue,
            };

            // Check limit price
            if solution.executed_buy_amount >= user_order.buy_amount
                && solution.executed_sell_amount <= user_order.sell_amount
            {
                match solution.into_settlement(&user_order) {
                    Ok(settlement) => settlements.push(settlement),
                    Err(err) => {
                        tracing::error!("baseline_solver failed to create settlement: {:?}", err)
                    }
                }
            }
        }

        settlements
    }

    fn settle_order(
        &self,
        order: &LimitOrder,
        pools: &HashMap<TokenPair, Vec<ConstantProductOrder>>,
    ) -> Option<Solution> {
        let candidates = path_candidates(
            order.sell_token,
            order.buy_token,
            &self.base_tokens,
            DEFAULT_MAX_HOPS,
        );

        let (path, executed_sell_amount, executed_buy_amount) = match order.kind {
            model::order::OrderKind::Buy => {
                let best = candidates
                    .iter()
                    .filter_map(|path| estimate_sell_amount(order.buy_amount, path, &pools))
                    .min_by_key(|estimate| estimate.value)?;
                (best.path, best.value, order.buy_amount)
            }
            model::order::OrderKind::Sell => {
                let best = candidates
                    .iter()
                    .filter_map(|path| estimate_buy_amount(order.sell_amount, path, &pools))
                    .max_by_key(|estimate| estimate.value)?;
                (best.path.clone(), order.sell_amount, best.value)
            }
        };
        Some(Solution {
            path: path.into_iter().cloned().collect(),
            executed_sell_amount,
            executed_buy_amount,
        })
    }

    #[cfg(test)]
    fn must_solve(&self, liquidity: Vec<Liquidity>) -> Settlement {
        self.solve(liquidity).into_iter().next().unwrap()
    }
}

struct Solution {
    path: Vec<ConstantProductOrder>,
    executed_sell_amount: U256,
    executed_buy_amount: U256,
}

impl Solution {
    fn into_settlement(self, order: &LimitOrder) -> Result<Settlement> {
        let mut settlement = Settlement::new(hashmap! {
            order.sell_token => self.executed_buy_amount,
            order.buy_token => self.executed_sell_amount,
        });

        settlement.with_liquidity(order, order.full_execution_amount())?;

        let (mut sell_amount, mut sell_token) = (self.executed_sell_amount, order.sell_token);
        for amm in self.path {
            let buy_token = amm.tokens.other(&sell_token).expect("Inconsistent path");
            let buy_amount = amm
                .get_amount_out(buy_token, sell_amount, sell_token)
                .expect("Path was found, so amount must be calculateable");
            settlement.with_liquidity(
                &amm,
                AmmOrderExecution {
                    input: (sell_token, sell_amount),
                    output: (buy_token, buy_amount),
                },
            )?;
            sell_amount = buy_amount;
            sell_token = buy_token;
        }

        Ok(settlement)
    }
}

fn amm_to_pool(amm: &ConstantProductOrder) -> Pool {
    Pool {
        tokens: amm.tokens,
        reserves: amm.reserves,
        fee: amm.fee,
    }
}

fn amm_to_weighted_pool(amm: &WeightedProductOrder) -> Result<WeightedPoolRef> {
    Ok(WeightedPoolRef {
        reserves: &amm.reserves,
        swap_fee_percentage: Bfp::try_from(&amm.fee)?,
    })
}

#[cfg(test)]
mod tests {
    use maplit::hashset;
    use model::order::OrderKind;
    use num::rational::Ratio;

    use crate::liquidity::{
        tests::CapturingSettlementHandler, AmmOrderExecution, ConstantProductOrder, LimitOrder,
    };

    use super::*;
    #[test]
    fn finds_best_route_sell_order() {
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(0);
        let native_token = H160::from_low_u64_be(3);

        let order_handler = vec![
            CapturingSettlementHandler::arc(),
            CapturingSettlementHandler::arc(),
        ];
        let orders = vec![
            LimitOrder {
                sell_amount: 100_000.into(),
                buy_amount: 100_000.into(),
                sell_token,
                buy_token,
                kind: OrderKind::Sell,
                partially_fillable: false,
                fee_amount: Default::default(),
                settlement_handling: order_handler[0].clone(),
                id: "0".into(),
            },
            // Second order has a more lax limit
            LimitOrder {
                sell_amount: 100_000.into(),
                buy_amount: 90_000.into(),
                buy_token,
                sell_token,
                kind: OrderKind::Sell,
                partially_fillable: false,
                fee_amount: Default::default(),
                settlement_handling: order_handler[1].clone(),
                id: "1".into(),
            },
        ];

        let amm_handler = vec![
            CapturingSettlementHandler::arc(),
            CapturingSettlementHandler::arc(),
            CapturingSettlementHandler::arc(),
        ];
        let amms = vec![
            ConstantProductOrder {
                tokens: TokenPair::new(buy_token, sell_token).unwrap(),
                reserves: (1_000_000, 1_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[0].clone(),
            },
            // Path via native token has more liquidity
            ConstantProductOrder {
                tokens: TokenPair::new(sell_token, native_token).unwrap(),
                reserves: (10_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[1].clone(),
            },
            // Second native token pool has a worse price despite larger k
            ConstantProductOrder {
                tokens: TokenPair::new(sell_token, native_token).unwrap(),
                reserves: (11_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[1].clone(),
            },
            ConstantProductOrder {
                tokens: TokenPair::new(native_token, buy_token).unwrap(),
                reserves: (10_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[2].clone(),
            },
        ];

        let mut liquidity: Vec<_> = orders.iter().cloned().map(Liquidity::Limit).collect();
        liquidity.extend(amms.iter().cloned().map(Liquidity::ConstantProduct));

        let solver = BaselineSolver::new(hashset! { native_token});
        let result = solver.must_solve(liquidity);
        assert_eq!(
            result.clearing_prices(),
            &hashmap! {
                sell_token => 97_459.into(),
                buy_token => 100_000.into(),
            }
        );

        // Second order is fully matched
        assert_eq!(order_handler[0].clone().calls().len(), 0);
        assert_eq!(order_handler[1].clone().calls()[0], 100_000.into());

        // Second & Third AMM are matched
        assert_eq!(amm_handler[0].clone().calls().len(), 0);
        assert_eq!(
            amm_handler[1].clone().calls()[0],
            AmmOrderExecution {
                input: (sell_token, 100_000.into()),
                output: (native_token, 98_715.into()),
            }
        );
        assert_eq!(
            amm_handler[2].clone().calls()[0],
            AmmOrderExecution {
                input: (native_token, 98_715.into()),
                output: (buy_token, 97_459.into()),
            }
        );
    }

    #[test]
    fn finds_best_route_buy_order() {
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(0);
        let native_token = H160::from_low_u64_be(3);

        let order_handler = vec![
            CapturingSettlementHandler::arc(),
            CapturingSettlementHandler::arc(),
        ];
        let orders = vec![
            LimitOrder {
                sell_amount: 100_000.into(),
                buy_amount: 100_000.into(),
                sell_token,
                buy_token,
                kind: OrderKind::Buy,
                partially_fillable: false,
                fee_amount: Default::default(),
                settlement_handling: order_handler[0].clone(),
                id: "0".into(),
            },
            // Second order has a more lax limit
            LimitOrder {
                sell_amount: 110_000.into(),
                buy_amount: 100_000.into(),
                buy_token,
                sell_token,
                kind: OrderKind::Buy,
                partially_fillable: false,
                fee_amount: Default::default(),
                settlement_handling: order_handler[1].clone(),
                id: "1".into(),
            },
        ];

        let amm_handler = vec![
            CapturingSettlementHandler::arc(),
            CapturingSettlementHandler::arc(),
            CapturingSettlementHandler::arc(),
        ];
        let amms = vec![
            ConstantProductOrder {
                tokens: TokenPair::new(buy_token, sell_token).unwrap(),
                reserves: (1_000_000, 1_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[0].clone(),
            },
            // Path via native token has more liquidity
            ConstantProductOrder {
                tokens: TokenPair::new(sell_token, native_token).unwrap(),
                reserves: (10_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[1].clone(),
            },
            // Second native token pool has a worse price despite larger k
            ConstantProductOrder {
                tokens: TokenPair::new(sell_token, native_token).unwrap(),
                reserves: (11_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[1].clone(),
            },
            ConstantProductOrder {
                tokens: TokenPair::new(native_token, buy_token).unwrap(),
                reserves: (10_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: amm_handler[2].clone(),
            },
        ];

        let mut liquidity: Vec<_> = orders.iter().cloned().map(Liquidity::Limit).collect();
        liquidity.extend(amms.iter().cloned().map(Liquidity::ConstantProduct));

        let solver = BaselineSolver::new(hashset! { native_token});
        let result = solver.must_solve(liquidity);
        assert_eq!(
            result.clearing_prices(),
            &hashmap! {
                sell_token => 100_000.into(),
                buy_token => 102_660.into(),
            }
        );

        // Second order is fully matched
        assert_eq!(order_handler[0].clone().calls().len(), 0);
        assert_eq!(order_handler[1].clone().calls()[0], 100_000.into());

        // Second & Third AMM are matched
        assert_eq!(amm_handler[0].clone().calls().len(), 0);
        assert_eq!(
            amm_handler[1].clone().calls()[0],
            AmmOrderExecution {
                input: (sell_token, 102_660.into()),
                output: (native_token, 101_315.into()),
            }
        );
        assert_eq!(
            amm_handler[2].clone().calls()[0],
            AmmOrderExecution {
                input: (native_token, 101_315.into()),
                output: (buy_token, 100_000.into()),
            }
        );
    }

    #[test]
    fn finds_best_route_when_pool_returns_none() {
        // Regression test for https://github.com/gnosis/gp-v2-services/issues/530
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(0);

        let orders = vec![LimitOrder {
            sell_amount: 110_000.into(),
            buy_amount: 100_000.into(),
            sell_token,
            buy_token,
            kind: OrderKind::Buy,
            partially_fillable: false,
            fee_amount: Default::default(),
            settlement_handling: CapturingSettlementHandler::arc(),
            id: "0".into(),
        }];

        let amms = vec![
            ConstantProductOrder {
                tokens: TokenPair::new(buy_token, sell_token).unwrap(),
                reserves: (10_000_000, 10_000_000),
                fee: Ratio::new(3, 1000),
                settlement_handling: CapturingSettlementHandler::arc(),
            },
            // Other direct pool has not enough liquidity to compute a valid estimate
            ConstantProductOrder {
                tokens: TokenPair::new(buy_token, sell_token).unwrap(),
                reserves: (0, 0),
                fee: Ratio::new(3, 1000),
                settlement_handling: CapturingSettlementHandler::arc(),
            },
        ];

        let mut liquidity: Vec<_> = orders.iter().cloned().map(Liquidity::Limit).collect();
        liquidity.extend(amms.iter().cloned().map(Liquidity::ConstantProduct));

        let solver = BaselineSolver::new(hashset! {});
        assert_eq!(solver.solve(liquidity).len(), 1);
    }
}
