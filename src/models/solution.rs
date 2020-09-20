use crate::models::Order;
use web3::types::U256;

#[derive(Debug)]
pub struct Solution {
    pub sell_orders_token0: Vec<Order>,
    pub sell_volumes_token0: Vec<U256>,
    pub sell_orders_token1: Vec<Order>,
    pub sell_volumes_token1: Vec<U256>,
}
