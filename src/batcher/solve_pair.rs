use crate::models::order::Order;
use crate::models::solution::Solution;
use anyhow::Result;
use web3::types::{U256, H256, Address};


pub enum OrderPairType {
    LhsFullyFilled,
    RhsFullyFilled,
    BothFullyFilled,
}


trait Matchable {
    /// Returns which of the two orders can be fully matched, or both, or none.
    fn match_compare(&self, other: &Order) -> Option<OrderPairType>;
    /// Returns whether the orders can be matched. For this the tokens need
    /// to match, there must be a price that satisfies both orders.
    fn attracts(&self, other: &Order) -> bool;
    /// Returns whether this order's sell token is the other order's buy token
    /// and vice versa.
    fn opposite_tokens(&self, other: &Order) -> bool;
    /// Returns whether there is a price that satisfies both orders.
    fn have_price_overlap(&self, other: &Order) -> bool;
}


impl Matchable for Order {
    fn match_compare(&self, other: &Order) -> Option<OrderPairType> {
        if !self.attracts(other) {
            return None;
        }

        if self.buy_amount <= other.sell_amount && self.sell_amount <= other.buy_amount {
            Some(OrderPairType::LhsFullyFilled)
        } else if self.buy_amount >= other.sell_amount && self.sell_amount >= other.buy_amount {
            Some(OrderPairType::RhsFullyFilled)
        } else {
            Some(OrderPairType::BothFullyFilled)
        }
    }

    fn attracts(&self, other: &Order) -> bool {
        self.opposite_tokens(other) && self.have_price_overlap(other)
    }

    fn opposite_tokens(&self, other: &Order) -> bool {
        self.buy_token == other.sell_token && self.sell_token == other.buy_token
    }

    fn have_price_overlap(&self, other: &Order) -> bool {
        self.sell_amount > U256::zero()
            && other.sell_amount > U256::zero()
            && self.buy_amount * other.buy_amount <= self.sell_amount * other.sell_amount
    }
}

struct Match {
    order_pair_type: OrderPairType,
    orders: OrderPair,
}

type OrderPair = [Order; 2];

type MatchedAmounts = [U256; 2];


pub fn solve_pair(
    sell_orders_token0: &Vec<Order>,
    sell_orders_token1: &Vec<Order>,
) -> Result<Solution> {
    // Can we assume that the orders are sorted already?
    assert!(check_orders_sorted_by_limit_price(&sell_orders_token0));
    assert!(check_orders_sorted_by_limit_price(&sell_orders_token1));
    // Or do we need to sort them first here?
    //sell_orders_token0.sort();
    //sell_orders_token1.sort();

    // Get number of orders in each direction.
    let nr_orders_token0 = sell_orders_token0.len();
    let nr_orders_token1 = sell_orders_token1.len();

    // Init vectors of sell amounts.
    let mut sell_volumes_token0 = vec![U256::zero(); nr_orders_token0];
    let mut sell_volumes_token1 = vec![U256::zero(); nr_orders_token1];

    // Match orders with best limit prices, if possible.
    if !(sell_orders_token0.is_empty() || sell_orders_token1.is_empty()) {

        // The best orders are the last elements in their vectors.
        let best_sell_order_token0 = &sell_orders_token0[nr_orders_token0 - 1];
        let best_sell_order_token1 = &sell_orders_token1[nr_orders_token1 - 1];

        if let Some(order_pair_type) = best_sell_order_token0.match_compare(
            &best_sell_order_token1
        ) {
            let matched_amounts = get_matched_amounts(
                &Match {
                    order_pair_type,
                    orders: [best_sell_order_token0.clone(), best_sell_order_token1.clone()],
                }
            );
            sell_volumes_token0[nr_orders_token0 - 1] = matched_amounts[0];
            sell_volumes_token1[nr_orders_token1 - 1] = matched_amounts[1];
        };
    };

    return Ok(Solution {
        sell_orders_token0: sell_orders_token0.clone(),
        sell_volumes_token0: sell_volumes_token0,
        sell_orders_token1: sell_orders_token1.clone(),
        sell_volumes_token1: sell_volumes_token1,
    });
}


fn get_matched_amounts(order_match: &Match) -> MatchedAmounts {

    let x = &order_match.orders[0];
    let y = &order_match.orders[1];

    match order_match.order_pair_type {
        OrderPairType::LhsFullyFilled => {
            [x.sell_amount, x.buy_amount]
        }
        OrderPairType::RhsFullyFilled => {
            [y.buy_amount, y.sell_amount]
        }
        OrderPairType::BothFullyFilled => {
            [x.sell_amount, y.sell_amount]
        }
    }
}


fn check_orders_sorted_by_limit_price(orders: &Vec<Order>) -> bool {
    for (i, x) in orders.iter().enumerate() {
        for y in orders.iter().skip(i + 1) {
            if x > y {
                return false
            }
        }
    }
    true
}


#[cfg(test)]
pub mod test_util {
    use super::*;
    use serde_json;

    #[test]
    fn test_check_orders_sorted_by_limit_price_false() {
        let mut orders: Vec<Order> = orders_unsorted();
        assert!(!check_orders_sorted_by_limit_price(&orders));

        orders.sort();
        assert!(check_orders_sorted_by_limit_price(&orders));
    }


    #[test]
    fn test_solve_pair_empty_orders() {
        let orders0: Vec<Order> = vec![];
        let orders1: Vec<Order> = vec![];
        let solution = solve_pair(&orders0, &orders1).unwrap();
        assert_eq!(solution.sell_volumes_token0, vec![]);
        assert_eq!(solution.sell_volumes_token1, vec![]);
    }


    #[test]
    #[should_panic]
    fn test_solve_pair_orders_unsorted() {
        let orders0: Vec<Order> = orders_unsorted();
        let orders1: Vec<Order> = vec![];
        solve_pair(&orders0, &orders1).unwrap();
    }


    #[test]
    fn test_solve_pair_no_match () {
        let orders0: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("120").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let orders1: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("120").unwrap(),
                sell_token: address(TOKEN1_ADDRESS),
                buy_token: address(TOKEN0_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let solution = solve_pair(&orders0, &orders1).unwrap();
        assert_eq!(solution.sell_volumes_token0, vec![U256::zero()]);
        assert_eq!(solution.sell_volumes_token1, vec![U256::zero()]);
    }

    #[test]
    fn test_solve_pair_match_lhs () {
        let orders0: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("80").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let orders1: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("200").unwrap(),
                buy_amount: U256::from_dec_str("160").unwrap(),
                sell_token: address(TOKEN1_ADDRESS),
                buy_token: address(TOKEN0_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let solution = solve_pair(&orders0, &orders1).unwrap();
        assert_eq!(solution.sell_volumes_token0, vec![U256::from_dec_str("100").unwrap()]);
        assert_eq!(solution.sell_volumes_token1, vec![U256::from_dec_str("80").unwrap()]);
    }

    #[test]
    fn test_solve_pair_match_rhs () {
        let orders0: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("200").unwrap(),
                buy_amount: U256::from_dec_str("160").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let orders1: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("80").unwrap(),
                sell_token: address(TOKEN1_ADDRESS),
                buy_token: address(TOKEN0_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let solution = solve_pair(&orders0, &orders1).unwrap();
        assert_eq!(solution.sell_volumes_token0, vec![U256::from_dec_str("80").unwrap()]);
        assert_eq!(solution.sell_volumes_token1, vec![U256::from_dec_str("100").unwrap()]);
    }

    #[test]
    fn test_solve_pair_match_both () {
        let orders0: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("80").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let orders1: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("80").unwrap(),
                sell_token: address(TOKEN1_ADDRESS),
                buy_token: address(TOKEN0_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let solution = solve_pair(&orders0, &orders1).unwrap();
        assert_eq!(solution.sell_volumes_token0, vec![U256::from_dec_str("100").unwrap()]);
        assert_eq!(solution.sell_volumes_token1, vec![U256::from_dec_str("100").unwrap()]);
    }

    #[test]
    fn test_solve_pair_match_best_orders () {
        let orders0: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("90").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            },
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("80").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let orders1: Vec<Order> = vec![
            Order {
                sell_amount: U256::from_dec_str("200").unwrap(),
                buy_amount: U256::from_dec_str("180").unwrap(),
                sell_token: address(TOKEN1_ADDRESS),
                buy_token: address(TOKEN0_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            },
            Order {
                sell_amount: U256::from_dec_str("200").unwrap(),
                buy_amount: U256::from_dec_str("160").unwrap(),
                sell_token: address(TOKEN1_ADDRESS),
                buy_token: address(TOKEN0_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ];
        let solution = solve_pair(&orders0, &orders1).unwrap();
        assert_eq!(solution.sell_volumes_token0, vec![U256::zero(), U256::from_dec_str("100").unwrap()]);
        assert_eq!(solution.sell_volumes_token1, vec![U256::zero(), U256::from_dec_str("80").unwrap()]);
    }


    // Define some constants for test orders.
    const TOKEN0_ADDRESS: &'static str = r#""0xA193E42526F1FEA8C99AF609dcEabf30C1c29fAA""#;
    const TOKEN1_ADDRESS: &'static str = r#""0xFDFEF9D10d929cB3905C71400ce6be1990EA0F34""#;
    const OWNER: &'static str = r#""0x63FC2aD3d021a4D7e64323529a55a9442C444dA0""#;

    const SIGNATURE_R: &'static str = r#""0x07cf23fa6f588cc3a91de8444b589e5afbf91c5d486c512a353d45d02fa58700""#;
    const SIGNATURE_S: &'static str = r#""0x53671e75b62b5bd64f91c80430aafb002040c35d1fcf25d0dc55d978946d5c11""#;

    fn address(address_str: &str) -> Address {
        return serde_json::from_str(address_str).unwrap()
    }

    fn signature(signature_str: &str) -> H256 {
        return serde_json::from_str(signature_str).unwrap()
    }

    fn orders_unsorted() -> Vec<Order> {
        vec![
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("50").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            },
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("80").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            },
            Order {
                sell_amount: U256::from_dec_str("100").unwrap(),
                buy_amount: U256::from_dec_str("20").unwrap(),
                sell_token: address(TOKEN0_ADDRESS),
                buy_token: address(TOKEN1_ADDRESS),
                owner: address(OWNER),
                nonce: 0,
                signature_v: 27 as u8,
                signature_r: signature(SIGNATURE_R),
                signature_s: signature(SIGNATURE_S),
                valid_until: U256::from("0")
            }
        ]
    }
}
