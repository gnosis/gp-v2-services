use crate::batcher::submit_solution::submit_solution;
use crate::models::orderbook::OrderBook;
use crate::solver::naive_solver::solve_pair;
use anyhow::Result;
use ethcontract::web3::types::Address;
use std::collections::HashMap;

pub async fn batch_process(orderbook: OrderBook) -> Result<()> {
    let tokens: Vec<Address> = orderbook
        .orders
        .read()
        .keys()
        .into_iter()
        .map(|t| Address::from(t.as_fixed_bytes()))
        .collect();

    let token_pairs = get_token_pairs(&tokens);
    for token_pair in token_pairs {
        let best_match = solve_pair(
            orderbook
                .orders
                .read()
                .get(&token_pair.0)
                .unwrap_or(&HashMap::new())
                .get(&token_pair.1)
                .unwrap_or(&Vec::new()),
            orderbook
                .orders
                .read()
                .get(&token_pair.1)
                .unwrap_or(&HashMap::new())
                .get(&token_pair.0)
                .unwrap_or(&Vec::new()),
        );
        submit_solution(best_match)?;
    }
    Ok(())
}

fn get_token_pairs(tokens: &Vec<Address>) -> Vec<(Address, Address)> {
    let last = match tokens.last() {
        Some(last_val) => last_val,
        None => return Vec::new(),
    };
    let mut tail = &tokens[1..];
    let mut token_pairs: Vec<(Address, Address)> = Vec::new();
    for i in tokens.iter() {
        for _ in tail.iter().map(|e| {
            token_pairs.push((*i, *e));
        }) {}

        if last != i {
            tail = &tail[1..];
        }
    }
    return token_pairs;
}

#[cfg(test)]
pub mod test_util {
    use super::*;

    #[test]
    fn test_get_token_pairs_with_two_tokens() {
        let token_1: Address = "A193E42526F1FEA8C99AF609dcEabf30C1c29fAA".parse().unwrap();
        let token_2: Address = "E193E42526F1FEA8C99AF609dcEabf30C1c29fAA".parse().unwrap();

        let expected = (token_1, token_2);
        let result = get_token_pairs(&vec![token_1, token_2]);

        assert_eq!(result[0], expected);
    }

    #[test]
    fn test_get_token_pairs_with_three_tokens() {
        let token_1: Address = "A193E42526F1FEA8C99AF609dcEabf30C1c29fAA".parse().unwrap();
        let token_2: Address = "B193E42526F1FEA8C99AF609dcEabf30C1c29fAA".parse().unwrap();
        let token_3: Address = "C193E42526F1FEA8C99AF609dcEabf30C1c29fAA".parse().unwrap();

        let expected = vec![(token_1, token_2), (token_1, token_3), (token_2, token_3)];
        let result = get_token_pairs(&vec![token_1, token_2, token_3]);

        assert_eq!(result, expected);
    }
}
