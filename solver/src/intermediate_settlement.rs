use std::{any::Any, collections::HashMap};

use anyhow::Result;
use ethcontract::{H160, U256};
use model::order::OrderKind;

use crate::{
    liquidity::{AmmOrder, AmmOrderExecution, LimitOrder},
    settlement::Settlement,
};

// An intermediate representation between SettledBatchAuctionModel and Settlement useful for
// postprocessing and doing the error checking up front and then working with a more
// convenient representation.
// NOTE: executed_liquidity_orders are assumed to be sorted according to a feasible execution plan.
#[derive(Debug)]
pub struct IntermediateSettlement {
    pub executed_limit_orders: Vec<ExecutedLimitOrder>,
    pub executed_liquidity_orders: Vec<ExecutedLiquidityOrder>,
    pub prices: HashMap<H160, U256>,
}

impl IntermediateSettlement {
    pub fn new(prices: HashMap<H160, U256>) -> Self {
        IntermediateSettlement {
            executed_limit_orders: Vec::new(),
            executed_liquidity_orders: Vec::new(),
            prices,
        }
    }
    pub fn add_executed_limit_order(&mut self, order: LimitOrder, executed_amount: U256) {
        let executed_limit_order = match order.kind {
            OrderKind::Sell => ExecutedLimitOrder {
                executed_buy_amount: executed_amount,
                executed_sell_amount: order.sell_amount,
                order,
            },
            OrderKind::Buy => ExecutedLimitOrder {
                executed_buy_amount: order.buy_amount,
                executed_sell_amount: executed_amount,
                order,
            },
        };
        self.executed_limit_orders.push(executed_limit_order);
    }
    pub fn add_fully_executed_limit_order(&mut self, order: LimitOrder) {
        let executed_limit_order = ExecutedLimitOrder {
            executed_buy_amount: order.buy_amount,
            executed_sell_amount: order.sell_amount,
            order,
        };
        self.executed_limit_orders.push(executed_limit_order);
    }
    pub fn add_executed_liquidity_order(
        &mut self,
        input: (H160, U256),
        output: (H160, U256),
        order: Box<dyn Any + Send + Sync>,
    ) {
        self.executed_liquidity_orders.push(ExecutedLiquidityOrder {
            order,
            input,
            output,
        });
    }
    pub fn price(&self, token: &H160) -> Option<U256> {
        self.prices.get(token).copied()
    }
    pub fn nr_executed_limit_orders(&self) -> usize {
        self.executed_limit_orders.len()
    }
    pub fn is_empty(&self) -> bool {
        self.executed_limit_orders.is_empty()
    }
    // O(n).
    pub fn executed_limit_order(&self, id: &str) -> Option<&ExecutedLimitOrder> {
        self.executed_limit_orders
            .iter()
            .find(|executed_limit_order| executed_limit_order.order.id == id)
    }
}

#[derive(Debug)]
pub struct ExecutedLimitOrder {
    pub order: LimitOrder,
    pub executed_buy_amount: U256,
    pub executed_sell_amount: U256,
}

impl ExecutedLimitOrder {
    pub fn executed_amount(&self) -> U256 {
        match self.order.kind {
            OrderKind::Buy => self.executed_buy_amount,
            OrderKind::Sell => self.executed_sell_amount,
        }
    }
}

#[derive(Debug)]
pub struct ExecutedLiquidityOrder {
    pub order: Box<dyn Any + Send + Sync>,
    pub input: (H160, U256),
    pub output: (H160, U256),
}

#[async_trait::async_trait(?Send)] // FIXME: Not sure what (?Send) does but it removes pages of rust errors!
pub trait SettlementFinalizing {
    async fn finalize_intermediate_settlement(
        &self,
        intermediate_settlement: IntermediateSettlement,
    ) -> Result<Settlement> {
        let mut settlement = Settlement::new(intermediate_settlement.prices);
        for order in intermediate_settlement.executed_limit_orders.iter() {
            self.finalize_intermediate_limit_order(&mut settlement, order)
                .await?;
        }
        for order in intermediate_settlement.executed_liquidity_orders.iter() {
            self.finalize_intermediate_liquidity_order(&mut settlement, order)
                .await?;
        }

        Ok(settlement)
    }
    async fn finalize_intermediate_limit_order(
        &self,
        settlement: &mut Settlement,
        order: &ExecutedLimitOrder,
    ) -> Result<()> {
        settlement.with_liquidity(&order.order, order.executed_amount())
    }
    async fn finalize_intermediate_liquidity_order(
        &self,
        settlement: &mut Settlement,
        order: &ExecutedLiquidityOrder,
    ) -> Result<()>;
}

pub struct AmmSettlementFinalizer {}

#[async_trait::async_trait(?Send)]
impl SettlementFinalizing for AmmSettlementFinalizer {
    async fn finalize_intermediate_liquidity_order(
        &self,
        settlement: &mut Settlement,
        order: &ExecutedLiquidityOrder,
    ) -> Result<()> {
        settlement.with_liquidity(
            order.order.downcast_ref::<AmmOrder>().unwrap(),
            AmmOrderExecution {
                input: order.input,
                output: order.output,
            },
        )
    }
}
