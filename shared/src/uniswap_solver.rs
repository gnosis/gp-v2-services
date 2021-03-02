use ethcontract::{H160, U256};
use model::TokenPair;
use std::collections::{HashMap, HashSet};

use crate::uniswap_pool::Pool;

type PathCandidate = Vec<TokenPair>;

pub fn estimate_buy_amount(
    sell_token: H160,
    sell_amount: U256,
    path: &[TokenPair],
    pools: &HashMap<TokenPair, Pool>,
) -> Option<U256> {
    path.iter()
        .fold(Some((sell_amount, sell_token)), |previous, pair| {
            let previous = match previous {
                Some(previous) => previous,
                None => return None,
            };

            match pools.get(pair) {
                Some(pool) => pool.get_amount_out(previous.1, previous.0),
                None => None,
            }
        })
        .map(|(amount, _)| amount)
}

pub fn estimate_sell_amount(
    buy_token: H160,
    buy_amount: U256,
    path: &[TokenPair],
    pools: &HashMap<TokenPair, Pool>,
) -> Option<U256> {
    path.iter()
        .rev()
        .fold(Some((buy_amount, buy_token)), |previous, pair| {
            let previous = match previous {
                Some(previous) => previous,
                None => return None,
            };

            match pools.get(pair) {
                Some(pool) => pool.get_amount_in(previous.1, previous.0),
                None => None,
            }
        })
        .map(|(amount, _)| amount)
}

pub fn path_candidates(
    sell_token: H160,
    buy_token: H160,
    base_tokens: &HashSet<H160>,
    max_hops: usize,
) -> HashSet<PathCandidate> {
    let mut candidates = HashSet::new();

    // Start with just the sell token (yields the direct pair candidate in the 0th iteration)
    let mut path_prefixes = vec![vec![sell_token]];
    for _ in 0..(max_hops + 1) {
        let mut next_round_path_prefixes = vec![];
        for path_prefix in &path_prefixes {
            // For this round, add the buy token and path to the candidates
            let mut full_path = path_prefix.clone();
            full_path.push(buy_token);
            candidates.insert(tokens_list_to_path_candidate(&full_path));

            // For the next round, amend current prefix with all base tokens that are not yet on the path
            for base_token in base_tokens {
                if base_token != &buy_token && !path_prefix.contains(base_token) {
                    let mut next_round_path_prefix = path_prefix.clone();
                    next_round_path_prefix.push(*base_token);
                    next_round_path_prefixes.push(next_round_path_prefix);
                }
            }
        }
        path_prefixes = next_round_path_prefixes;
    }
    candidates
}

fn tokens_list_to_path_candidate(token_list: &[H160]) -> PathCandidate {
    token_list
        .windows(2)
        .map(|window| {
            TokenPair::new(window[0], window[1]).expect("token list contains same token in a row")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethcontract::H160;
    use maplit::{hashmap, hashset};
    use model::TokenPair;
    use std::iter::FromIterator;

    #[test]
    fn test_path_candidates() {
        let base_tokens = vec![
            H160::from_low_u64_be(0),
            H160::from_low_u64_be(1),
            H160::from_low_u64_be(2),
        ];
        let base_token_set = &HashSet::from_iter(base_tokens.clone());

        let sell_token = H160::from_low_u64_be(4);
        let buy_token = H160::from_low_u64_be(5);

        // 0 hops
        assert_eq!(
            path_candidates(sell_token, buy_token, base_token_set, 0),
            hashset! {vec![TokenPair::new(sell_token, buy_token).unwrap()]}
        );

        // 1 hop with all permutations
        assert_eq!(
            path_candidates(sell_token, buy_token, base_token_set, 1),
            hashset! {
                vec![TokenPair::new(sell_token, buy_token).unwrap()],
                vec![TokenPair::new(sell_token, base_tokens[0]).unwrap(), TokenPair::new(base_tokens[0], buy_token).unwrap()],
                vec![TokenPair::new(sell_token, base_tokens[1]).unwrap(), TokenPair::new(base_tokens[1], buy_token).unwrap()],
                vec![TokenPair::new(sell_token, base_tokens[2]).unwrap(), TokenPair::new(base_tokens[2], buy_token).unwrap()],

            }
        );

        // 2 & 3 hops check count
        assert_eq!(
            path_candidates(sell_token, buy_token, base_token_set, 2).len(),
            10
        );
        assert_eq!(
            path_candidates(sell_token, buy_token, base_token_set, 3).len(),
            16
        );

        // 4 hops should not yield any more permutations since we used all base tokens
        assert_eq!(
            path_candidates(sell_token, buy_token, base_token_set, 4).len(),
            16
        );

        // Ignores base token if part of buy or sell
        assert_eq!(
            path_candidates(base_tokens[0], buy_token, base_token_set, 1),
            hashset! {
                vec![TokenPair::new(base_tokens[0], buy_token).unwrap()],
                vec![TokenPair::new(base_tokens[0], base_tokens[1]).unwrap(), TokenPair::new(base_tokens[1], buy_token).unwrap()],
                vec![TokenPair::new(base_tokens[0], base_tokens[2]).unwrap(), TokenPair::new(base_tokens[2], buy_token).unwrap()],

            }
        );
        assert_eq!(
            path_candidates(sell_token, base_tokens[0], base_token_set, 1),
            hashset! {
                vec![TokenPair::new(sell_token, base_tokens[0]).unwrap()],
                vec![TokenPair::new(sell_token, base_tokens[1]).unwrap(), TokenPair::new(base_tokens[1], base_tokens[0]).unwrap()],
                vec![TokenPair::new(sell_token, base_tokens[2]).unwrap(), TokenPair::new(base_tokens[2], base_tokens[0]).unwrap()],

            }
        );
    }

    #[test]
    fn test_estimate_amount_returns_none_if_it_contains_pair_without_pool() {
        let sell_token = H160::from_low_u64_be(1);
        let intermediate = H160::from_low_u64_be(2);
        let buy_token = H160::from_low_u64_be(3);

        let path = vec![
            TokenPair::new(sell_token, intermediate).unwrap(),
            TokenPair::new(intermediate, buy_token).unwrap(),
        ];
        let pools = hashmap! {
            path[0] => Pool::uniswap(path[0], (100, 100)),
        };

        assert_eq!(
            estimate_buy_amount(sell_token, 1.into(), &path, &pools),
            None
        );
        assert_eq!(
            estimate_sell_amount(sell_token, 1.into(), &path, &pools),
            None
        );
    }

    #[test]
    fn test_estimate_amount() {
        let sell_token = H160::from_low_u64_be(1);
        let intermediate = H160::from_low_u64_be(2);
        let buy_token = H160::from_low_u64_be(3);

        let path = vec![
            TokenPair::new(sell_token, intermediate).unwrap(),
            TokenPair::new(intermediate, buy_token).unwrap(),
        ];
        let pools = hashmap! {
            path[0] => Pool::uniswap(path[0],(100, 100)),
            path[1] => Pool::uniswap(path[1],(200, 50)),
        };

        assert_eq!(
            estimate_buy_amount(sell_token, 10.into(), &path, &pools),
            Some(2.into())
        );

        assert_eq!(
            estimate_sell_amount(buy_token, 10.into(), &path, &pools),
            Some(105.into())
        );
    }

    #[test]
    fn test_estimate_sell_amount_returns_none_buying_too_much() {
        let sell_token = H160::from_low_u64_be(1);
        let intermediate = H160::from_low_u64_be(2);
        let buy_token = H160::from_low_u64_be(3);

        let path = vec![
            TokenPair::new(sell_token, intermediate).unwrap(),
            TokenPair::new(intermediate, buy_token).unwrap(),
        ];
        let pools = hashmap! {
            path[0] => Pool::uniswap(path[0],(100, 100)),
            path[1] => Pool::uniswap(path[1],(200, 50)),
        };

        assert_eq!(
            estimate_sell_amount(buy_token, 100.into(), &path, &pools),
            None
        );
    }
}
