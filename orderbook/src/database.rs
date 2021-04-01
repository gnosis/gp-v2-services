mod events;
mod fees;
mod orders;
mod trades;

use anyhow::Result;
use sqlx::PgPool;

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
        self.pool
            .execute(sqlx::query("TRUNCATE settlements;"))
            .await?;
        Ok(())
    }

    pub async fn drop_orders(&self, table_name: &str) -> Result<()> {
        use sqlx::Executor;
        self.pool.execute(sqlx::query("TRUNCATE orders;")).await?;
        Ok(())
    }
}
