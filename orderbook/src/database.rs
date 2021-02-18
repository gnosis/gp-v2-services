mod events;
mod fees;
mod orders;
mod solver_orders;
mod trades;

use anyhow::Result;
use model::order::OrderKind;
use sqlx::PgPool;
use std::convert::TryInto;

pub use events::*;
pub use orders::OrderFilter;
pub use trades::TradeFilter;

// TODO: There is remaining optimization potential by implementing sqlx encoding and decoding for
// U256 directly instead of going through BigDecimal. This is not very important as this is fast
// enough anyway.

// The pool uses an Arc internally.
#[derive(Clone)]
pub struct Database {
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

impl Database {
    pub fn new(uri: &str) -> Result<Self> {
        Ok(Self {
            pool: PgPool::connect_lazy(uri)?,
        })
    }

    /// Delete all data in the database. Only used by tests.
    pub async fn clear(&self) -> Result<()> {
        use sqlx::Executor;
        self.pool.execute(sqlx::query("TRUNCATE orders;")).await?;
        self.pool.execute(sqlx::query("TRUNCATE trades;")).await?;
        self.pool
            .execute(sqlx::query("TRUNCATE invalidations;"))
            .await?;
        self.pool
            .execute(sqlx::query("TRUNCATE min_fee_measurements;"))
            .await?;
        Ok(())
    }
}

#[derive(sqlx::Type)]
#[sqlx(rename = "OrderKind")]
#[sqlx(rename_all = "lowercase")]
enum DbOrderKind {
    Buy,
    Sell,
}

impl DbOrderKind {
    fn from(order_kind: OrderKind) -> Self {
        match order_kind {
            OrderKind::Buy => Self::Buy,
            OrderKind::Sell => Self::Sell,
        }
    }

    fn into(self) -> OrderKind {
        match self {
            Self::Buy => OrderKind::Buy,
            Self::Sell => OrderKind::Sell,
        }
    }
}
