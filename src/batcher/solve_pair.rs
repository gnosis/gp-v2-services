use crate::models::order::Order;
use crate::models::solution::Solution;
use anyhow::Result;
use web3::types::{U256, H256, Address};


trait Matchable {
    /// Returns whether there is a price that satisfies both orders.
    fn have_price_overlap(&self, other: &Order) -> bool;
}


impl Matchable for Order {
    fn have_price_overlap(&self, other: &Order) -> bool {
        self.sell_amount > U256::zero()
            && other.sell_amount > U256::zero()
            && self.buy_amount * other.buy_amount <= self.sell_amount * other.sell_amount
    }
}


pub fn solve_pair(
    sell_orders_token0: &Vec<Order>,
    sell_orders_token1: &Vec<Order>,
) -> Result<Solution> {
    assert!(check_orders_sorted_by_limit_price(&sell_orders_token0));
    assert!(check_orders_sorted_by_limit_price(&sell_orders_token1));
    
    // Get number of orders in each direction.
    let nr_orders_token0 = sell_orders_token0.len();
    let nr_orders_token1 = sell_orders_token1.len();

    // Init vectors of sell amounts.
    let mut executed_sell_orders_token0: Vec<Order> = vec![];
    let mut executed_sell_orders_token1: Vec<Order> = vec![];

    // Match orders with best limit prices, if possible.
    if !(sell_orders_token0.is_empty() || sell_orders_token1.is_empty()) {

        // The best orders are the last elements in their vectors.
        let best_sell_order_token0 = &sell_orders_token0[nr_orders_token0 - 1];
        let best_sell_order_token1 = &sell_orders_token1[nr_orders_token1 - 1];

        if best_sell_order_token0.have_price_overlap(&best_sell_order_token1) {
            executed_sell_orders_token0.push(best_sell_order_token0.clone());
            executed_sell_orders_token1.push(best_sell_order_token1.clone());
        };
    };

    return Ok(Solution {
        sell_orders_token0: executed_sell_orders_token0,
        sell_orders_token1: executed_sell_orders_token1,
    });
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
        assert_eq!(solution.sell_orders_token0, vec![]);
        assert_eq!(solution.sell_orders_token1, vec![]);
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
        assert_eq!(solution.sell_orders_token0, vec![]);
        assert_eq!(solution.sell_orders_token1, vec![]);
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
        assert_eq!(solution.sell_orders_token0, orders0);
        assert_eq!(solution.sell_orders_token1, orders1);
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
        assert_eq!(solution.sell_orders_token0, orders0);
        assert_eq!(solution.sell_orders_token1, orders1);
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
        assert_eq!(solution.sell_orders_token0, orders0);
        assert_eq!(solution.sell_orders_token1, orders1);
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
        assert_eq!(solution.sell_orders_token0, vec![orders0[1].clone()]);
        assert_eq!(solution.sell_orders_token1, vec![orders1[1].clone()]);
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
