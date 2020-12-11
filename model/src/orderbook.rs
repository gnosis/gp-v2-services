use crate::Order;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait OrderbookReading {
    // Read all "valid" orders (according to the implementation's definition of validity)
    async fn get_orders(&self) -> Result<Vec<Order>>;
}
