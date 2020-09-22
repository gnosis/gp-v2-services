use crate::models::order::Order;
use crate::models::solution::Solution;
use anyhow::Result;
use web3::types::U256;

pub fn solve_pair(
    sell_orders_token0: &Vec<Order>,
    sell_orders_token1: &Vec<Order>,
) -> Result<Solution> {
    return Ok(Solution {
        sell_orders_token0: sell_orders_token0.clone(),
        sell_volumes_token0: vec![U256::one()],
        sell_orders_token1: sell_orders_token1.clone(),
        sell_volumes_token1: vec![U256::one()],
    });
}
