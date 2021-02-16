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
#[derive(Debug, Default)]
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
    use crate::database::{Event, EventIndex};
    use ethcontract::U256;
    use futures::StreamExt;
    use model::order::{Order, OrderCreation, OrderMetaData};
    use model::trade::{DbTrade, Trade};
    use num_bigint::BigUint;
    use std::collections::HashSet;

    #[tokio::test]
    #[ignore]
    async fn postgres_trade_roundtrip() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();
        let filter = TradeFilter::default();
        assert!(db.trades(&filter).boxed().next().await.is_none());

        // Common fields
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);
        let owner = H160::from_low_u64_be(3);
        let executed_buy_amount = 7u32;
        let executed_sell_amount = 3u32;
        let fee_amount = 2u32;
        let uid = OrderUid([1u8; 56]);

        let trade = Trade {
            block_number: 2,
            log_index: 0,
            order_uid: uid,
            sell_amount: BigUint::from(executed_sell_amount),
            buy_amount: BigUint::from(executed_buy_amount),
            sell_amount_before_fees: BigUint::from(executed_sell_amount - fee_amount),
            owner,
            buy_token,
            sell_token,
        };
        let order = Order {
            order_meta_data: OrderMetaData {
                owner,
                uid,
                executed_sell_amount_before_fees: BigUint::from(executed_sell_amount),
                executed_fee_amount: BigUint::from(fee_amount),
                executed_buy_amount: BigUint::from(executed_buy_amount),
                ..Default::default()
            },
            order_creation: OrderCreation {
                sell_token,
                buy_token,
                ..Default::default()
            },
        };

        // Add order and trade event
        db.insert_order(&order).await.unwrap();
        db.insert_events(vec![(
            EventIndex {
                block_number: 2,
                log_index: 0,
            },
            Event::DbTrade(DbTrade {
                order_uid: uid,
                sell_amount_including_fee: executed_sell_amount.into(),
                buy_amount: executed_buy_amount.into(),
                fee_amount: fee_amount.into(),
            }),
        )])
        .await
        .unwrap();
        assert_eq!(
            db.trades(&filter)
                .try_collect::<Vec<Trade>>()
                .await
                .unwrap(),
            vec![trade]
        );
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_filter_trades_by_owner() {
        let db = Database::new("postgresql://").unwrap();
        db.clear().await.unwrap();

        let owners: Vec<H160> = [0, 1, 2]
            .iter()
            .map(|t| H160::from_low_u64_be(*t as u64))
            .collect();
        let tokens: Vec<H160> = [0, 1, 2, 3]
            .iter()
            .map(|t| H160::from_low_u64_be(*t as u64))
            .collect();
        let uids: Vec<OrderUid> = [0, 1, 2].iter().map(|i| OrderUid([*i; 56])).collect();
        let order_ids = [OrderUid([0; 56]), OrderUid([1u8; 56]), OrderUid([2u8; 56])];

        // Create some orders.
        let orders = vec![
            Order {
                order_meta_data: OrderMetaData {
                    owner: owners[0],
                    uid: uids[0],
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
        ];

        db.insert_events(vec![
            (
                EventIndex {
                    block_number: 2,
                    log_index: 0,
                },
                Event::DbTrade(DbTrade {
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
                Event::DbTrade(DbTrade {
                    order_uid: order_ids[2],
                    sell_amount_including_fee: U256::from(4),
                    buy_amount: U256::from(3),
                    fee_amount: U256::from(2),
                }),
            ),
        ])
        .await
        .unwrap();

        async fn assert_trades(db: &Database, filter: &TradeFilter, expected: &[Trade]) {
            let filtered = db
                .trades(&filter)
                .try_collect::<HashSet<Trade>>()
                .await
                .unwrap();
            let expected = expected.iter().cloned().collect::<HashSet<_>>();
            assert_eq!(filtered, expected);
        }

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
            &trades[1..],
        )
        .await;
    }
    //
    // #[tokio::test]
    // #[ignore]
    // async fn postgres_filter_orders_by_fully_executed() {
    //     let db = Database::new("postgresql://").unwrap();
    //     db.clear().await.unwrap();
    //
    //     let order = Order {
    //         order_meta_data: Default::default(),
    //         order_creation: OrderCreation {
    //             kind: OrderKind::Sell,
    //             sell_amount: 10.into(),
    //             buy_amount: 100.into(),
    //             ..Default::default()
    //         },
    //     };
    //     db.insert_order(&order).await.unwrap();
    //
    //     let get_order = |exclude_fully_executed| {
    //         let db = db.clone();
    //         async move {
    //             db.orders(&OrderFilter {
    //                 exclude_fully_executed,
    //                 ..Default::default()
    //             })
    //                 .boxed()
    //                 .next()
    //                 .await
    //         }
    //     };
    //
    //     let order = get_order(true).await.unwrap().unwrap();
    //     assert_eq!(
    //         order.order_meta_data.executed_sell_amount,
    //         BigUint::from(0u8)
    //     );
    //
    //     db.insert_events(vec![(
    //         EventIndex {
    //             block_number: 0,
    //             log_index: 0,
    //         },
    //         Event::DbTrade(DbTrade {
    //             order_uid: order.order_meta_data.uid,
    //             sell_amount_including_fee: 3.into(),
    //             ..Default::default()
    //         }),
    //     )])
    //         .await
    //         .unwrap();
    //     let order = get_order(true).await.unwrap().unwrap();
    //     assert_eq!(
    //         order.order_meta_data.executed_sell_amount,
    //         BigUint::from(3u8)
    //     );
    //
    //     db.insert_events(vec![(
    //         EventIndex {
    //             block_number: 1,
    //             log_index: 0,
    //         },
    //         Event::DbTrade(DbTrade {
    //             order_uid: order.order_meta_data.uid,
    //             sell_amount_including_fee: 6.into(),
    //             ..Default::default()
    //         }),
    //     )])
    //         .await
    //         .unwrap();
    //     let order = get_order(true).await.unwrap().unwrap();
    //     assert_eq!(
    //         order.order_meta_data.executed_sell_amount,
    //         BigUint::from(9u8),
    //     );
    //
    //     // The order disappears because it is fully executed.
    //     db.insert_events(vec![(
    //         EventIndex {
    //             block_number: 2,
    //             log_index: 0,
    //         },
    //         Event::DbTrade(DbTrade {
    //             order_uid: order.order_meta_data.uid,
    //             sell_amount_including_fee: 1.into(),
    //             ..Default::default()
    //         }),
    //     )])
    //         .await
    //         .unwrap();
    //     assert!(get_order(true).await.is_none());
    //
    //     // If we include fully executed orders it is there.
    //     let order = get_order(false).await.unwrap().unwrap();
    //     assert_eq!(
    //         order.order_meta_data.executed_sell_amount,
    //         BigUint::from(10u8)
    //     );
    //
    //     // Change order type and see that is returned as not fully executed again.
    //     let query = "UPDATE orders SET kind = 'buy';";
    //     db.pool.execute(query).await.unwrap();
    //     assert!(get_order(true).await.is_some());
    // }
    //
    // // In the schema we set the type of executed amounts in individual events to a 78 decimal digit
    // // number. Summing over multiple events could overflow this because the smart contract only
    // // guarantees that the filled amount (which amount that is depends on order type) does not
    // // overflow a U256. This test shows that postgres does not error if this happens because
    // // inside the SUM the number can have more digits.
    // #[tokio::test]
    // #[ignore]
    // async fn postgres_summed_executed_amount_does_not_overflow() {
    //     let db = Database::new("postgresql://").unwrap();
    //     db.clear().await.unwrap();
    //
    //     let order = Order {
    //         order_meta_data: Default::default(),
    //         order_creation: OrderCreation {
    //             kind: OrderKind::Sell,
    //             ..Default::default()
    //         },
    //     };
    //     db.insert_order(&order).await.unwrap();
    //
    //     for i in 0..10 {
    //         db.insert_events(vec![(
    //             EventIndex {
    //                 block_number: i,
    //                 log_index: 0,
    //             },
    //             Event::DbTrade(DbTrade {
    //                 order_uid: order.order_meta_data.uid,
    //                 sell_amount_including_fee: U256::MAX,
    //                 ..Default::default()
    //             }),
    //         )])
    //             .await
    //             .unwrap();
    //     }
    //
    //     let order = db
    //         .orders(&OrderFilter::default())
    //         .boxed()
    //         .next()
    //         .await
    //         .unwrap()
    //         .unwrap();
    //
    //     let expected = u256_to_big_uint(&U256::MAX) * BigUint::from(10u8);
    //     assert!(expected.to_string().len() > 78);
    //     assert_eq!(order.order_meta_data.executed_sell_amount, expected);
    // }
    //
    // #[tokio::test]
    // #[ignore]
    // async fn postgres_filter_orders_by_invalidated() {
    //     let db = Database::new("postgresql://").unwrap();
    //     db.clear().await.unwrap();
    //     let uid = OrderUid([0u8; 56]);
    //     let order = Order {
    //         order_meta_data: OrderMetaData {
    //             uid,
    //             ..Default::default()
    //         },
    //         ..Default::default()
    //     };
    //     db.insert_order(&order).await.unwrap();
    //
    //     let is_order_valid = || async {
    //         db.orders(&OrderFilter {
    //             exclude_invalidated: true,
    //             ..Default::default()
    //         })
    //             .boxed()
    //             .next()
    //             .await
    //             .transpose()
    //             .unwrap()
    //             .is_some()
    //     };
    //
    //     assert!(is_order_valid().await);
    //
    //     // Invalidating a different order doesn't affect first order.
    //     sqlx::query(
    //         "INSERT INTO invalidations (block_number, log_index, order_uid) VALUES ($1, $2, $3)",
    //     )
    //         .bind(0i64)
    //         .bind(0i64)
    //         .bind([1u8; 56].as_ref())
    //         .execute(&db.pool)
    //         .await
    //         .unwrap();
    //     assert!(is_order_valid().await);
    //
    //     // But invalidating it does work
    //     sqlx::query(
    //         "INSERT INTO invalidations (block_number, log_index, order_uid) VALUES ($1, $2, $3)",
    //     )
    //         .bind(1i64)
    //         .bind(0i64)
    //         .bind([0u8; 56].as_ref())
    //         .execute(&db.pool)
    //         .await
    //         .unwrap();
    //     assert!(!is_order_valid().await);
    //
    //     // And we can invalidate it several times.
    //     sqlx::query(
    //         "INSERT INTO invalidations (block_number, log_index, order_uid) VALUES ($1, $2, $3)",
    //     )
    //         .bind(2i64)
    //         .bind(0i64)
    //         .bind([0u8; 56].as_ref())
    //         .execute(&db.pool)
    //         .await
    //         .unwrap();
    //     assert!(!is_order_valid().await);
    // }
}
