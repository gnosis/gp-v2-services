use crate::models::solution::Solution;
use crate::models::Order;
use ethcontract::web3::types::U256;

trait Matchable {
    /// Returns whether there is a price that satisfies both orders.
    fn have_price_overlap(&self, other: &Order) -> bool;
}

impl Matchable for Order {
    fn have_price_overlap(&self, other: &Order) -> bool {
        self.get_current_sell_amount() > U256::zero()
            && other.get_current_sell_amount() > U256::zero()
            && self.get_current_buy_amount() * other.get_current_buy_amount()
                <= self.get_current_sell_amount() * other.get_current_sell_amount()
    }
}

pub fn solve_pair(sell_orders_token0: &Vec<Order>, sell_orders_token1: &Vec<Order>) -> Solution {
    assert!(check_orders_sorted_by_limit_price(&sell_orders_token0));
    assert!(check_orders_sorted_by_limit_price(&sell_orders_token1));

    // Match orders with best limit prices, if possible.
    if let (Some(best_sell_order_token0), Some(best_sell_order_token1)) =
        (sell_orders_token0.last(), sell_orders_token1.last())
    {
        if best_sell_order_token0.have_price_overlap(&best_sell_order_token1) {
            return Solution {
                sell_orders_token0: vec![best_sell_order_token0.clone()],
                sell_orders_token1: vec![best_sell_order_token1.clone()],
            };
        };
    };

    Solution {
        sell_orders_token0: vec![],
        sell_orders_token1: vec![],
    }
}

fn check_orders_sorted_by_limit_price(orders: &Vec<Order>) -> bool {
    for (i, x) in orders.iter().enumerate() {
        for y in orders.iter().skip(i + 1) {
            if x > y {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
pub mod test_util {
    use super::*;
    use ethcontract::web3::types::{Address, H256};

    // Define some constants for test orders.
    const TOKEN0_ADDRESS: &'static str = "A193E42526F1FEA8C99AF609dcEabf30C1c29fAA";
    const TOKEN1_ADDRESS: &'static str = "FDFEF9D10d929cB3905C71400ce6be1990EA0F34";
    const OWNER: &'static str = "63FC2aD3d021a4D7e64323529a55a9442C444dA0";

    const SIGNATURE_R: &'static str =
        "07cf23fa6f588cc3a91de8444b589e5afbf91c5d486c512a353d45d02fa58700";
    const SIGNATURE_S: &'static str =
        "53671e75b62b5bd64f91c80430aafb002040c35d1fcf25d0dc55d978946d5c11";

    fn generate_standard_order_with_defined_sell_and_buy_amount(
        sell_amount: &str,
        buy_amount: &str,
    ) -> Order {
        Order {
            sell_amount: U256::from_dec_str(sell_amount).unwrap(),
            buy_amount: U256::from_dec_str(buy_amount).unwrap(),
            current_sell_amount: None,
            current_buy_amount: None,
            sell_token: address(TOKEN0_ADDRESS),
            buy_token: address(TOKEN1_ADDRESS),
            owner: address(OWNER),
            nonce: 0,
            signature_v: 27 as u8,
            signature_r: signature(SIGNATURE_R),
            signature_s: signature(SIGNATURE_S),
            valid_until: U256::from("0"),
        }
    }
    fn generate_standard_opposite_order_with_defined_sell_and_buy_amount(
        sell_amount: &str,
        buy_amount: &str,
    ) -> Order {
        Order {
            sell_amount: U256::from_dec_str(sell_amount).unwrap(),
            buy_amount: U256::from_dec_str(buy_amount).unwrap(),
            current_sell_amount: None,
            current_buy_amount: None,
            sell_token: address(TOKEN1_ADDRESS),
            buy_token: address(TOKEN0_ADDRESS),
            owner: address(OWNER),
            nonce: 0,
            signature_v: 27 as u8,
            signature_r: signature(SIGNATURE_R),
            signature_s: signature(SIGNATURE_S),
            valid_until: U256::from("0"),
        }
    }
    fn address(address_str: &str) -> Address {
        return (address_str).parse().unwrap();
    }

    fn signature(signature_str: &str) -> H256 {
        return (signature_str).parse().unwrap();
    }
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
        let solution = solve_pair(&orders0, &orders1);
        assert_eq!(solution.sell_orders_token0, vec![]);
        assert_eq!(solution.sell_orders_token1, vec![]);
    }

    #[test]
    #[should_panic]
    fn test_solve_pair_orders_unsorted() {
        let orders0: Vec<Order> = orders_unsorted();
        let orders1: Vec<Order> = vec![];
        solve_pair(&orders0, &orders1);
    }

    #[test]
    fn test_solve_pair_no_match() {
        let orders0: Vec<Order> = vec![generate_standard_order_with_defined_sell_and_buy_amount(
            "100", "120",
        )];
        let orders1: Vec<Order> =
            vec![generate_standard_opposite_order_with_defined_sell_and_buy_amount("100", "120")];
        let solution = solve_pair(&orders0, &orders1);
        assert_eq!(solution.sell_orders_token0, vec![]);
        assert_eq!(solution.sell_orders_token1, vec![]);
    }

    #[test]
    fn test_solve_pair_match_lhs() {
        let orders0: Vec<Order> = vec![generate_standard_order_with_defined_sell_and_buy_amount(
            "100", "80",
        )];
        let orders1: Vec<Order> =
            vec![generate_standard_opposite_order_with_defined_sell_and_buy_amount("200", "160")];
        let solution = solve_pair(&orders0, &orders1);
        assert_eq!(solution.sell_orders_token0, orders0);
        assert_eq!(solution.sell_orders_token1, orders1);
    }

    #[test]
    fn test_solve_pair_match_rhs() {
        let orders0: Vec<Order> = vec![generate_standard_order_with_defined_sell_and_buy_amount(
            "200", "160",
        )];
        let orders1: Vec<Order> =
            vec![generate_standard_opposite_order_with_defined_sell_and_buy_amount("100", "80")];
        let solution = solve_pair(&orders0, &orders1);
        assert_eq!(solution.sell_orders_token0, orders0);
        assert_eq!(solution.sell_orders_token1, orders1);
    }

    #[test]
    fn test_solve_pair_match_both() {
        let orders0: Vec<Order> = vec![generate_standard_order_with_defined_sell_and_buy_amount(
            "100", "80",
        )];
        let orders1: Vec<Order> =
            vec![generate_standard_opposite_order_with_defined_sell_and_buy_amount("100", "80")];
        let solution = solve_pair(&orders0, &orders1);
        assert_eq!(solution.sell_orders_token0, orders0);
        assert_eq!(solution.sell_orders_token1, orders1);
    }

    #[test]
    fn test_solve_pair_match_best_orders() {
        let orders0: Vec<Order> = vec![
            generate_standard_order_with_defined_sell_and_buy_amount("100", "90"),
            generate_standard_order_with_defined_sell_and_buy_amount("100", "80"),
        ];
        let orders1: Vec<Order> = vec![
            generate_standard_opposite_order_with_defined_sell_and_buy_amount("200", "180"),
            generate_standard_opposite_order_with_defined_sell_and_buy_amount("200", "160"),
        ];
        let solution = solve_pair(&orders0, &orders1);
        assert_eq!(solution.sell_orders_token0, vec![orders0[1].clone()]);
        assert_eq!(solution.sell_orders_token1, vec![orders1[1].clone()]);
    }

    fn orders_unsorted() -> Vec<Order> {
        vec![
            generate_standard_order_with_defined_sell_and_buy_amount("100", "50"),
            generate_standard_order_with_defined_sell_and_buy_amount("100", "80"),
            generate_standard_order_with_defined_sell_and_buy_amount("100", "20"),
        ]
    }
}
