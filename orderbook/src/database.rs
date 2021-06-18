mod events;
mod fees;
mod orders;
mod trades;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use contracts::gpv2_settlement::Event as ContractEvent;
use ethcontract::Event as EthContractEvent;
use futures::{stream::BoxStream, StreamExt};
use model::{
    order::{Order, OrderKind, OrderUid},
    trade::Trade as ModelTrade,
};
use primitive_types::{H160, U256};
use shared::{event_handling::EventIndex, maintenance::Maintaining};
use sqlx::{Executor, PgPool, Row};
use std::collections::HashMap;

pub use events::*;
pub use orders::OrderFilter;
pub use trades::TradeFilter;

use crate::fee::MinFeeStoring;

#[async_trait::async_trait]
pub trait Database: Send + Sync {
    async fn clear(&self) -> Result<()>;
    async fn count_rows_in_tables(&self) -> Result<HashMap<&'static str, i64>>;

    async fn block_number_of_most_recent_event(&self) -> Result<u64>;
    async fn insert_events(&self, events: Vec<(EventIndex, Event)>) -> Result<()>;
    async fn replace_events(
        &self,
        delete_from_block_number: u64,
        events: Vec<(EventIndex, Event)>,
    ) -> Result<()>;
    fn contract_to_db_events(
        &self,
        contract_events: Vec<EthContractEvent<ContractEvent>>,
    ) -> Result<Vec<(EventIndex, Event)>>;

    async fn insert_order(&self, order: &Order) -> Result<(), InsertionError>;
    async fn cancel_order(&self, order_uid: &OrderUid, now: DateTime<Utc>) -> Result<()>;
    fn orders<'a>(&'a self, filter: &'a OrderFilter) -> BoxStream<'a, Result<Order>>;

    fn trades<'a>(&'a self, filter: &'a TradeFilter) -> BoxStream<'a, Result<ModelTrade>>;

    async fn save_fee_measurement(
        &self,
        sell_token: H160,
        buy_token: Option<H160>,
        amount: Option<U256>,
        kind: Option<OrderKind>,
        expiry: DateTime<Utc>,
        min_fee: U256,
    ) -> Result<()>;
    async fn get_min_fee(
        &self,
        sell_token: H160,
        buy_token: Option<H160>,
        amount: Option<U256>,
        kind: Option<OrderKind>,
        min_expiry: DateTime<Utc>,
    ) -> Result<Option<U256>>;
    async fn remove_expired_fee_measurements(&self, max_expiry: DateTime<Utc>) -> Result<()>;
}

// TODO: There is remaining optimization potential by implementing sqlx encoding and decoding for
// U256 directly instead of going through BigDecimal. This is not very important as this is fast
// enough anyway.

// The names of all tables we use in the db.
const ALL_TABLES: [&str; 5] = [
    "orders",
    "trades",
    "invalidations",
    "min_fee_measurements",
    "settlements",
];

// The pool uses an Arc internally.
#[derive(Clone)]
pub struct Postgres {
    pool: PgPool,
}

#[derive(Debug)]
pub enum InsertionError {
    DuplicatedRecord,
    DbError(sqlx::Error),
}

impl From<sqlx::Error> for InsertionError {
    fn from(err: sqlx::Error) -> Self {
        Self::DbError(err)
    }
}

// The implementation is split up into several modules which contain more public methods.

impl Postgres {
    pub fn new(uri: &str) -> Result<Self> {
        Ok(Self {
            pool: PgPool::connect_lazy(uri)?,
        })
    }

    /// Delete all data in the database. Only used by tests.
    async fn clear_(&self) -> Result<()> {
        for table in ALL_TABLES.iter() {
            self.pool
                .execute(format!("TRUNCATE {};", table).as_str())
                .await?;
        }
        Ok(())
    }

    async fn count_rows_in_table(&self, table: &str) -> Result<i64> {
        let query = format!("SELECT COUNT(*) FROM {};", table);
        let row = self.pool.fetch_one(query.as_str()).await?;
        row.try_get(0).map_err(Into::into)
    }

    async fn count_rows_in_tables_(&self) -> Result<HashMap<&'static str, i64>> {
        let mut result = HashMap::new();
        for &table in ALL_TABLES.iter() {
            result.insert(table, self.count_rows_in_table(table).await?);
        }
        Ok(result)
    }
}

#[async_trait::async_trait]
impl Database for Postgres {
    async fn clear(&self) -> Result<()> {
        self.clear_().await
    }

    async fn count_rows_in_tables(&self) -> Result<HashMap<&'static str, i64>> {
        self.count_rows_in_tables_().await
    }

    async fn block_number_of_most_recent_event(&self) -> Result<u64> {
        self.block_number_of_most_recent_event_().await
    }

    async fn insert_events(&self, events: Vec<(EventIndex, Event)>) -> Result<()> {
        self.insert_events_(events).await
    }

    async fn replace_events(
        &self,
        delete_from_block_number: u64,
        events: Vec<(EventIndex, Event)>,
    ) -> Result<()> {
        self.replace_events_(delete_from_block_number, events).await
    }

    fn contract_to_db_events(
        &self,
        contract_events: Vec<EthContractEvent<ContractEvent>>,
    ) -> Result<Vec<(EventIndex, Event)>> {
        self.contract_to_db_events_(contract_events)
    }

    async fn insert_order(&self, order: &Order) -> Result<(), InsertionError> {
        self.insert_order_(order).await
    }

    async fn cancel_order(&self, order_uid: &OrderUid, now: DateTime<Utc>) -> Result<()> {
        self.cancel_order_(order_uid, now).await
    }

    fn orders<'a>(&'a self, filter: &'a OrderFilter) -> BoxStream<'a, Result<Order>> {
        self.orders_(filter).boxed()
    }

    fn trades<'a>(&'a self, filter: &'a TradeFilter) -> BoxStream<'a, Result<ModelTrade>> {
        self.trades_(filter).boxed()
    }

    async fn save_fee_measurement(
        &self,
        sell_token: H160,
        buy_token: Option<H160>,
        amount: Option<U256>,
        kind: Option<OrderKind>,
        expiry: DateTime<Utc>,
        min_fee: U256,
    ) -> Result<()> {
        self.save_fee_measurement_(sell_token, buy_token, amount, kind, expiry, min_fee)
            .await
    }

    async fn get_min_fee(
        &self,
        sell_token: H160,
        buy_token: Option<H160>,
        amount: Option<U256>,
        kind: Option<OrderKind>,
        min_expiry: DateTime<Utc>,
    ) -> Result<Option<U256>> {
        self.get_min_fee_(sell_token, buy_token, amount, kind, min_expiry)
            .await
    }

    async fn remove_expired_fee_measurements(&self, max_expiry: DateTime<Utc>) -> Result<()> {
        self.remove_expired_fee_measurements_(max_expiry).await
    }
}

#[async_trait::async_trait]
impl Maintaining for Postgres {
    async fn run_maintenance(&self) -> Result<()> {
        self.remove_expired_fee_measurements(Utc::now())
            .await
            .context("fee measurement maintenance error")
    }
}

#[async_trait::async_trait]
impl MinFeeStoring for Postgres {
    async fn save_fee_measurement(
        &self,
        sell_token: H160,
        buy_token: Option<H160>,
        amount: Option<U256>,
        kind: Option<OrderKind>,
        expiry: DateTime<Utc>,
        min_fee: U256,
    ) -> Result<()> {
        Database::save_fee_measurement(self, sell_token, buy_token, amount, kind, expiry, min_fee)
            .await
    }

    async fn get_min_fee(
        &self,
        sell_token: H160,
        buy_token: Option<H160>,
        amount: Option<U256>,
        kind: Option<OrderKind>,
        min_expiry: DateTime<Utc>,
    ) -> Result<Option<U256>> {
        Database::get_min_fee(self, sell_token, buy_token, amount, kind, min_expiry).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn postgres_count_rows_in_tables_works() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();

        let counts = db.count_rows_in_tables().await.unwrap();
        assert_eq!(counts.len(), 5);
        assert!(counts.iter().all(|(_, count)| *count == 0));

        db.insert_order(&Default::default()).await.unwrap();
        let counts = db.count_rows_in_tables().await.unwrap();
        assert_eq!(counts.get("orders"), Some(&1));
    }
}
