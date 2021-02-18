use super::*;
use crate::conversions::*;
use anyhow::{anyhow, Context, Result};
use bigdecimal::BigDecimal;
use futures::{stream::TryStreamExt, Stream};
use model::{
    order::{OrderCreation, OrderKind, SolverOrder},
    Signature,
};

impl Database {
    pub fn solver_orders(&self, min_valid_to: u32) -> impl Stream<Item = Result<SolverOrder>> + '_ {
        const QUERY: &str = "\
        SELECT * FROM ( \
            SELECT \
                o.owner, o.sell_token, o.buy_token, o.sell_amount, o.buy_amount, o.valid_to, \
                o.app_data, o.fee_amount, o.kind, o.partially_fillable, o.signature, \
                COALESCE(SUM(CASE o.kind \
                    WHEN 'sell' THEN t.sell_amount - t.fee_amount \
                    WHEN 'buy' THEN t.buy_amount \
                END), 0) AS executed_amount \
            FROM orders o \
            WHERE o.cancellation_timestasmp IS NULL
            LEFT OUTER JOIN trades t ON o.uid = t.order_uid \
            LEFT OUTER JOIN invalidations ON o.uid = invalidations.order_uid \
            GROUP BY o.uid \
            HAVING COUNT(invalidations.*) = 0
        ) AS temp \
        WHERE executed_amount < (CASE kind
            WHEN 'sell' THEN sell_amount \
            WHEN 'buy' THEN buy_amount \
        END) \
        ;";
        sqlx::query_as(QUERY)
            .bind(min_valid_to)
            .fetch(&self.pool)
            .err_into()
            .and_then(|row: OrdersQueryRow| async move { row.into_order() })
    }
}

#[derive(sqlx::FromRow)]
struct OrdersQueryRow {
    sell_token: Vec<u8>,
    buy_token: Vec<u8>,
    sell_amount: BigDecimal,
    buy_amount: BigDecimal,
    valid_to: i64,
    app_data: i64,
    fee_amount: BigDecimal,
    kind: DbOrderKind,
    partially_fillable: bool,
    signature: Vec<u8>,
    executed_amount: BigDecimal,
}

impl OrdersQueryRow {
    fn into_order(self) -> Result<SolverOrder> {
        let order_creation = OrderCreation {
            sell_token: h160_from_vec(self.sell_token)?,
            buy_token: h160_from_vec(self.buy_token)?,
            sell_amount: big_decimal_to_u256(&self.sell_amount)
                .ok_or_else(|| anyhow!("sell_amount is not U256"))?,
            buy_amount: big_decimal_to_u256(&self.buy_amount)
                .ok_or_else(|| anyhow!("buy_amount is not U256"))?,
            valid_to: self.valid_to.try_into().context("valid_to is not u32")?,
            app_data: self.app_data.try_into().context("app_data is not u32")?,
            fee_amount: big_decimal_to_u256(&self.fee_amount)
                .ok_or_else(|| anyhow!("buy_amount is not U256"))?,
            kind: self.kind.into(),
            partially_fillable: self.partially_fillable,
            signature: Signature::from_bytes(
                &self
                    .signature
                    .try_into()
                    .map_err(|_| anyhow!("signature has wrong length"))?,
            ),
        };
        let executable_amount = if self.partially_fillable {
            Some(
                match order_creation.kind {
                    OrderKind::Buy => order_creation.buy_amount,
                    OrderKind::Sell => order_creation.sell_amount,
                } - big_decimal_to_u256(&self.executed_amount)
                    .ok_or_else(|| anyhow!("executed_amount is not U256"))?,
            )
        } else {
            None
        };
        Ok(SolverOrder {
            order_creation,
            executable_amount,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn postgres_solver_orders_query_works() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        db.solver_orders(0).try_collect::<Vec<_>>().await.unwrap();
    }

    // TODO: more tests
}
