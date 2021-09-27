use crate::solver::solver_utils::Slippage;
use crate::solver::zeroex_solver::api::SwapQuery;
use crate::solver::zeroex_solver::api::SwapResponse;
use crate::solver::zeroex_solver::STANDARD_ZEROEX_SLIPPAGE_BPS;
use crate::solver::{Auction, Solver};
use crate::solver::{ParaswapSolver, ZeroExSolver};
use crate::{liquidity::LimitOrder, settlement::Settlement};
use anyhow::{anyhow, Result};
use contracts::GPv2Settlement;
use ethcontract::{Account, H160, U256};
use futures::future::join_all;
use reqwest::Client;
use shared::{token_info::TokenInfoFetching, Web3};
use std::collections::HashMap;
use std::sync::Arc;

pub struct CowDexAgSolver {
    pub account: Account,
    pub paraswap: ParaswapSolver,
    pub zeroex: ZeroExSolver,
}

impl CowDexAgSolver {
    pub fn new(
        account: Account,
        web3: Web3,
        settlement_contract: GPv2Settlement,
        token_info: Arc<dyn TokenInfoFetching>,
        slippage_bps: u32,
        disabled_paraswap_dexs: Vec<String>,
        client: Client,
        partner: Option<String>,
        chain_id: u64,
    ) -> Self {
        CowDexAgSolver {
            account: account.clone(),
            paraswap: ParaswapSolver::new(
                account.clone(),
                web3.clone(),
                settlement_contract.clone(),
                token_info,
                slippage_bps,
                disabled_paraswap_dexs,
                client.clone(),
                partner,
            ),
            zeroex: ZeroExSolver::new(account, web3, settlement_contract, chain_id, client)
                .unwrap(),
        }
    }
}

#[async_trait::async_trait]
impl Solver for CowDexAgSolver {
    async fn solve(&self, Auction { orders, .. }: Auction) -> Result<Vec<Settlement>> {
        if orders.len() == 0 {
            return Ok(Vec::new());
        }
        // Randomize which orders we start with to prevent us getting stuck on bad orders.
        // orders.shuffle(&mut rand::thread_rng())
        let mut orders: Vec<LimitOrder> = orders.into_iter().collect();
        // for now, lets only solve for 5 orders
        orders.truncate(5);

        // Step1: get trade amounts from each order via paraswap dex-ag
        let futures = orders.iter().map(|order| async move {
            let token_info = self
                .paraswap
                .token_info
                .get_token_infos(&[order.sell_token, order.buy_token])
                .await;
            let (price_response, _amount) = match self
                .paraswap
                .get_full_price_info_for_order(&order, &token_info)
                .await
            {
                Ok(response) => response,
                Err(err) => {
                    tracing::debug!("Could not get price for order {:?}: {:?}", order, err);
                    return vec![];
                }
            };
            let mut single_trades = Vec::new();
            if price_response.price_route.dest_amount.gt(&order.buy_amount) {
                for swap in &price_response.price_route.best_route.get(0).unwrap().swaps {
                    for trade in &swap.swap_exchanges {
                        println!("trade: {:?}", trade);
                        let src_token = over_write_eth_with_weth_token(swap.src_token);
                        let dest_token = over_write_eth_with_weth_token(swap.dest_token);
                        single_trades.push((
                            src_token,
                            dest_token,
                            trade.src_amount,
                            trade.dest_amount,
                        ));
                    }
                }
            }
            single_trades
        });
        let results: Vec<(H160, H160, U256, U256)> =
            join_all(futures).await.into_iter().flatten().collect();
        println!("{:?}", results);
        let mut trade_amounts: HashMap<(H160, H160), (U256, U256)> = HashMap::new();
        for (src_token, dest_token, src_amount, dest_amount) in results {
            trade_amounts
                .entry((src_token, dest_token))
                .and_modify(|(in_amounts, out_amounts)| {
                    (
                        in_amounts.checked_add(src_amount).unwrap(),
                        out_amounts.checked_add(dest_amount).unwrap(),
                    );
                })
                .or_insert((src_amount, dest_amount));
        }

        for (pair, entry_amouts) in &trade_amounts {
            println!(
                " Before cow merge: trade on pair {:?} with values {:?}",
                pair, entry_amouts
            );
        }

        // 2nd step: Removing obvious cow volume from traded amounts
        let mut updated_traded_amounts = HashMap::new();
        for (pair, entry_amouts) in trade_amounts.clone() {
            let (src_token, dest_token) = pair;
            if updated_traded_amounts.get(&pair).is_some()
                || updated_traded_amounts
                    .get(&(dest_token, src_token))
                    .is_some()
            {
                continue;
            }
            if let Some(opposite_amounts) = trade_amounts.get(&(dest_token, src_token)) {
                if entry_amouts.1.gt(&opposite_amounts.clone().0) {
                    updated_traded_amounts.insert(
                        (dest_token, src_token),
                        (
                            entry_amouts.1.checked_sub(opposite_amounts.0).unwrap(),
                            U256::zero(),
                        ),
                    );
                } else if entry_amouts.0.gt(&opposite_amounts.clone().1) {
                    updated_traded_amounts.insert(
                        pair,
                        (
                            entry_amouts.0.checked_sub(opposite_amounts.1).unwrap(),
                            U256::zero(),
                        ),
                    );
                } else {
                    return Err(anyhow!("Not sure how to proceed, cow too good"));
                }
            } else {
                updated_traded_amounts.insert(pair, trade_amounts.get(&pair).unwrap().clone());
            }
        }
        for (pair, entry_amouts) in &updated_traded_amounts {
            println!(
                " After cow merge: trade on pair {:?} with values {:?}",
                pair, entry_amouts
            );
        }

        // 3rd step: Get trades of left over amounts from zeroEx
        let mut settlement: Settlement = Settlement::new(HashMap::new());
        let futures = updated_traded_amounts
            .into_iter()
            .map(|(pair, entry_amouts)| async move {
                let (src_token, dest_token) = pair;
                let query = SwapQuery {
                    sell_token: src_token,
                    buy_token: dest_token,
                    sell_amount: Some(entry_amouts.0),
                    buy_amount: None,
                    slippage_percentage: Slippage::number_from_basis_points(
                        STANDARD_ZEROEX_SLIPPAGE_BPS,
                    )
                    .unwrap(),
                    skip_validation: Some(true),
                };
                (query.clone(), self.zeroex.client.get_swap(query).await)
            });
        let mut swap_results = join_all(futures).await;
        // 4th step: Build settlements
        // todo: Insert clearing prices smarter with less failures
        while swap_results.len() > 0 {
            let (query, swap) = swap_results.pop().unwrap();
            let swap = match swap {
                Ok(swap) => swap,
                Err(err) => return Err(anyhow!("Could not get zeroX trade, due to {:}", err)),
            };
            insert_new_price(&mut settlement, &trade_amounts, query.clone(), swap.clone())?;
            let spender = swap.allowance_target;
            settlement.encoder.append_to_execution_plan(
                self.zeroex
                    .allowance_fetcher
                    .get_approval(query.sell_token, spender, swap.sell_amount)
                    .await?,
            );
            settlement.encoder.append_to_execution_plan(swap);
        }
        for order in orders {
            settlement.with_liquidity(&order, order.full_execution_amount())?;
        }
        Ok(vec![settlement])
    }
    fn account(&self) -> &Account {
        &self.account
    }

    fn name(&self) -> &'static str {
        "CowDexAgSolver"
    }
}
fn over_write_eth_with_weth_token(token: H160) -> H160 {
    if token.eq(&"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".parse().unwrap()) {
        "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".parse().unwrap()
    } else {
        token
    }
}

pub fn insert_new_price(
    settlement: &mut Settlement,
    trade_amounts: &HashMap<(H160, H160), (U256, U256)>,
    query: SwapQuery,
    swap: SwapResponse,
) -> Result<()> {
    let src_token = query.sell_token;
    let dest_token = query.buy_token;
    let (sell_amount, buy_amount) = match (
        trade_amounts.get(&(src_token, dest_token)),
        trade_amounts.get(&(dest_token, src_token)),
    ) {
        (Some((_sell_amount, _)), Some((buy_amount, substracted_sell_amount))) => {
            (*substracted_sell_amount, buy_amount.clone())
        }
        (Some((_, _)), None) => (U256::zero(), U256::zero()),
        _ => return Err(anyhow!("This case should not happen, please investigate")),
    };
    let (sell_amount, buy_amount) = (
        sell_amount.checked_add(swap.sell_amount).unwrap(),
        buy_amount.checked_add(swap.buy_amount).unwrap(),
    );

    match (
        settlement.clearing_price(query.sell_token),
        settlement.clearing_price(query.buy_token),
    ) {
        (Some(_), Some(_)) => return Err(anyhow!("can't deal with such a ring")),
        (Some(price_sell_token), None) => {
            settlement.encoder.insert_new_clearing_price(
                price_sell_token
                    .checked_mul(buy_amount)
                    .unwrap()
                    .checked_div(sell_amount)
                    .unwrap(),
                query.buy_token,
            );
            settlement.encoder.insert_new_token(query.buy_token);
        }
        (None, Some(price_buy_token)) => {
            settlement.encoder.insert_new_clearing_price(
                price_buy_token
                    .checked_mul(sell_amount)
                    .unwrap()
                    .checked_div(buy_amount)
                    .unwrap(),
                query.sell_token,
            );
            settlement.encoder.insert_new_token(query.sell_token);
        }
        (None, None) => {
            settlement
                .encoder
                .insert_new_clearing_price(buy_amount, query.sell_token);
            settlement.encoder.insert_new_token(query.sell_token);

            settlement
                .encoder
                .insert_new_clearing_price(sell_amount, query.buy_token);
            settlement.encoder.insert_new_token(query.buy_token);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::account;
    use model::order::Order;
    use model::order::OrderCreation;
    use model::order::OrderKind;
    use reqwest::Client;
    use shared::{token_info::TokenInfoFetcher, transport::create_env_test_transport};
    #[tokio::test]
    // #[ignore]
    async fn solve_with_star_solver_dai_gno_weth() {
        let web3 = Web3::new(create_env_test_transport());
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();
        let token_info_fetcher = Arc::new(TokenInfoFetcher { web3: web3.clone() });
        let dai = shared::addr!("6b175474e89094c44da98b954eedeac495271d0f");
        let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");
        let weth = shared::addr!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

        let solver = CowDexAgSolver::new(
            account(),
            web3,
            settlement,
            token_info_fetcher,
            1,
            vec![],
            Client::new(),
            None,
            1u64,
        );

        let dai_gno_order: LimitOrder = Order {
            order_creation: OrderCreation {
                sell_token: dai,
                buy_token: gno,
                sell_amount: 1_100_000_000_000_000_000_000u128.into(),
                buy_amount: 1u128.into(),
                kind: OrderKind::Sell,
                ..Default::default()
            },
            ..Default::default()
        }
        .into();
        let gno_weth_order: LimitOrder = Order {
            order_creation: OrderCreation {
                sell_token: gno,
                buy_token: weth,
                sell_amount: 10_000_000_000_000_000_000u128.into(),
                buy_amount: 1u128.into(),
                kind: OrderKind::Sell,
                ..Default::default()
            },
            ..Default::default()
        }
        .into();
        let settlement = solver
            .solve(Auction {
                orders: vec![dai_gno_order, gno_weth_order],
                ..Default::default()
            })
            .await
            .unwrap();

        println!("{:#?}", settlement);
    }

    #[tokio::test]
    // #[ignore]
    async fn solve_with_star_solver_bal_gno_weth() {
        let web3 = Web3::new(create_env_test_transport());
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();
        let token_info_fetcher = Arc::new(TokenInfoFetcher { web3: web3.clone() });
        let dai = shared::addr!("6b175474e89094c44da98b954eedeac495271d0f");
        let bal = shared::addr!("ba100000625a3754423978a60c9317c58a424e3d");
        let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");

        let solver = CowDexAgSolver::new(
            account(),
            web3,
            settlement,
            token_info_fetcher,
            1,
            vec![],
            Client::new(),
            None,
            1u64,
        );

        let dai_gno_order: LimitOrder = Order {
            order_creation: OrderCreation {
                sell_token: dai,
                buy_token: gno,
                sell_amount: 11_000_000_000_000_000_000_000u128.into(),
                buy_amount: 1u128.into(),
                kind: OrderKind::Sell,
                ..Default::default()
            },
            ..Default::default()
        }
        .into();
        let bal_dai_order: LimitOrder = Order {
            order_creation: OrderCreation {
                sell_token: gno,
                buy_token: bal,
                sell_amount: 1_000_000_000_000_000_000_000u128.into(),
                buy_amount: 1u128.into(),
                kind: OrderKind::Sell,
                ..Default::default()
            },
            ..Default::default()
        }
        .into();
        let settlement = solver
            .solve(Auction {
                orders: vec![bal_dai_order, dai_gno_order],
                ..Default::default()
            })
            .await
            .unwrap();

        println!("{:#?}", settlement);
    }
}
