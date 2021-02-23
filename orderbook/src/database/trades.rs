use crate::conversions::{big_decimal_to_big_uint, h160_from_vec};
use crate::database::Database;
use anyhow::{anyhow, Context, Result};
use bigdecimal::BigDecimal;
use ethcontract::H160;
use futures::{stream::TryStreamExt, Stream};
use model::order::OrderUid;
use model::trade::Trade;
use std::convert::TryInto;

/// Any default value means that this field is unfiltered.
#[derive(Debug, Default, PartialEq)]
pub struct TradeFilter {
    pub owner: Option<H160>,
    pub order_uid: Option<OrderUid>,
}

impl Database {
    pub fn trades<'a>(&'a self, filter: &'a TradeFilter) -> impl Stream<Item = Result<Trade>> + 'a {
        const QUERY: &str = "\
            SELECT \
                t.block_number, \
                t.log_index, \
                t.order_uid, \
                t.buy_amount, \
                t.sell_amount, \
                t.sell_amount - t.fee_amount as sell_amount_before_fees,\
                o.owner, \
                o.buy_token, \
                o.sell_token \
            FROM trades t \
            JOIN orders o \
            ON o.uid = t.order_uid \
            WHERE \
                o.uid IS NOT null \
            AND \
                ($1 IS NULL OR o.owner = $1) \
            AND \
                ($2 IS NULL OR o.uid = $2);";

        sqlx::query_as(QUERY)
            .bind(filter.owner.as_ref().map(|h160| h160.as_bytes()))
            .bind(filter.order_uid.as_ref().map(|uid| uid.0.as_ref()))
            .fetch(&self.pool)
            .err_into()
            .and_then(|row: TradesQueryRow| async move { row.into_trade() })
    }
}

#[derive(sqlx::FromRow)]
struct TradesQueryRow {
    block_number: i64,
    log_index: i64,
    order_uid: Vec<u8>,
    buy_amount: BigDecimal,
    sell_amount: BigDecimal,
    sell_amount_before_fees: BigDecimal,
    owner: Vec<u8>,
    buy_token: Vec<u8>,
    sell_token: Vec<u8>,
}

impl TradesQueryRow {
    fn into_trade(self) -> Result<Trade> {
        let block_number = self
            .block_number
            .try_into()
            .context("block_number is not u32")?;
        let log_index = self.log_index.try_into().context("log_index is not u32")?;
        let order_uid = OrderUid(
            self.order_uid
                .try_into()
                .map_err(|_| anyhow!("order uid has wrong length"))?,
        );
        let buy_amount = big_decimal_to_big_uint(&self.buy_amount)
            .ok_or_else(|| anyhow!("buy_amount is not an unsigned integer"))?;
        let sell_amount = big_decimal_to_big_uint(&self.sell_amount)
            .ok_or_else(|| anyhow!("sell_amount is not an unsigned integer"))?;
        let sell_amount_before_fees = big_decimal_to_big_uint(&self.sell_amount_before_fees)
            .ok_or_else(|| anyhow!("sell_amount_before_fees is not an unsigned integer"))?;
        let owner = h160_from_vec(self.owner)?;
        let buy_token = h160_from_vec(self.buy_token)?;
        let sell_token = h160_from_vec(self.sell_token)?;
        Ok(Trade {
            block_number,
            log_index,
            order_uid,
            buy_amount,
            sell_amount,
            sell_amount_before_fees,
            owner,
            buy_token,
            sell_token,
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::database::{Event, EventIndex, Trade as DbTrade};
    use ethcontract::U256;
    use model::order::{Order, OrderCreation, OrderMetaData};
    use model::trade::Trade;
    use num_bigint::BigUint;
    use std::collections::HashSet;

    async fn populate_dummy_trade_db(db: Database) -> (Vec<H160>, Vec<OrderUid>, [Trade; 3]) {
        // Common values
        let owners: Vec<H160> = [0, 1, 2]
            .iter()
            .map(|t| H160::from_low_u64_be(*t as u64))
            .collect();
        let tokens: Vec<H160> = [0, 1, 2, 3]
            .iter()
            .map(|t| H160::from_low_u64_be(*t as u64))
            .collect();
        let order_ids: Vec<OrderUid> = [0, 1, 2, 3].iter().map(|i| OrderUid([*i; 56])).collect();

        // Create some orders.
        let orders = vec![
            Order {
                order_meta_data: OrderMetaData {
                    owner: owners[0],
                    uid: order_ids[0],
                    ..Default::default()
                },
                order_creation: OrderCreation {
                    sell_token: tokens[1],
                    buy_token: tokens[2],
                    valid_to: 10,
                    ..Default::default()
                },
            },
            Order {
                order_meta_data: OrderMetaData {
                    owner: owners[0],
                    uid: order_ids[1],
                    ..Default::default()
                },
                order_creation: OrderCreation {
                    sell_token: tokens[2],
                    buy_token: tokens[3],
                    valid_to: 11,
                    ..Default::default()
                },
            },
            Order {
                order_meta_data: OrderMetaData {
                    owner: owners[2],
                    uid: order_ids[2],
                    ..Default::default()
                },
                order_creation: OrderCreation {
                    sell_token: tokens[2],
                    buy_token: tokens[3],
                    valid_to: 12,
                    ..Default::default()
                },
            },
        ];
        for order in orders.iter() {
            db.insert_order(order).await.unwrap();
        }

        let trade_without_order = Trade {
            block_number: 2,
            log_index: 0,
            order_uid: order_ids[3],
            sell_amount: BigUint::from(9u32),
            buy_amount: BigUint::from(9u32),
            sell_amount_before_fees: BigUint::from(1u32),
            owner: owners[0],
            buy_token: tokens[3],
            sell_token: tokens[2],
        };

        let trades = [
            Trade {
                block_number: 2,
                log_index: 0,
                order_uid: order_ids[1],
                sell_amount: BigUint::from(3u32),
                buy_amount: BigUint::from(2u32),
                sell_amount_before_fees: BigUint::from(1u32),
                owner: owners[0],
                buy_token: tokens[3],
                sell_token: tokens[2],
            },
            Trade {
                block_number: 2,
                log_index: 1,
                order_uid: order_ids[2],
                sell_amount: BigUint::from(4u32),
                buy_amount: BigUint::from(3u32),
                sell_amount_before_fees: BigUint::from(2u32),
                owner: owners[2],
                buy_token: tokens[3],
                sell_token: tokens[2],
            },
            trade_without_order,
        ];

        db.insert_events(vec![
            (
                EventIndex {
                    block_number: 2,
                    log_index: 0,
                },
                Event::Trade(DbTrade {
                    order_uid: order_ids[1],
                    sell_amount_including_fee: U256::from(3),
                    buy_amount: U256::from(2),
                    fee_amount: U256::from(2),
                }),
            ),
            (
                EventIndex {
                    block_number: 2,
                    log_index: 1,
                },
                Event::Trade(DbTrade {
                    order_uid: order_ids[2],
                    sell_amount_including_fee: U256::from(4),
                    buy_amount: U256::from(3),
                    fee_amount: U256::from(2),
                }),
            ),
        ])
        .await
        .unwrap();

        return (owners, order_ids, trades);
    }

    async fn assert_trades(db: &Database, filter: &TradeFilter, expected: &[Trade]) {
        let filtered = db
            .trades(&filter)
            .try_collect::<HashSet<Trade>>()
            .await
            .unwrap();
        let expected = expected.iter().cloned().collect::<HashSet<_>>();
        assert_eq!(filtered, expected);
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_trades_without_filter() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let (_, _, trades) = populate_dummy_trade_db(db.clone()).await;
        assert_trades(&db, &TradeFilter::default(), &trades[0..2]).await;
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_trades_with_owner_filter() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let (owners, _, trades) = populate_dummy_trade_db(db.clone()).await;
        assert_trades(
            &db,
            &TradeFilter {
                owner: Some(owners[1]),
                ..Default::default()
            },
            &[],
        )
        .await;

        assert_trades(
            &db,
            &TradeFilter {
                owner: Some(owners[0]),
                ..Default::default()
            },
            &trades[0..1],
        )
        .await;

        assert_trades(
            &db,
            &TradeFilter {
                owner: Some(owners[2]),
                ..Default::default()
            },
            &trades[1..2],
        )
        .await;
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_trades_with_order_uid_filter() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();

        let (_, order_ids, trades) = populate_dummy_trade_db(db.clone()).await;
        assert_trades(
            &db,
            &TradeFilter {
                order_uid: Some(order_ids[0]),
                ..Default::default()
            },
            &[],
        )
        .await;

        assert_trades(
            &db,
            &TradeFilter {
                order_uid: Some(order_ids[1]),
                ..Default::default()
            },
            &trades[0..1],
        )
        .await;

        assert_trades(
            &db,
            &TradeFilter {
                order_uid: Some(order_ids[2]),
                ..Default::default()
            },
            &trades[1..2],
        )
        .await;
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_trade_without_matching_order() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();

        let (_owners, order_ids, _trades) = populate_dummy_trade_db(db.clone()).await;

        // Trade exists in DB but no matching order
        assert_trades(
            &db,
            &TradeFilter {
                order_uid: Some(order_ids[3]),
                ..Default::default()
            },
            &[],
        )
        .await;
    }
}
