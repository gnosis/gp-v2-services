use crate::batcher::solve_pair::solve_pair;
use crate::batcher::submit_solution::submit_solution;
use crate::models::orderbook::OrderBook;
use crate::models::token_list::TokenList;
use std::collections::HashMap;
use web3::types::Address;

pub async fn batch_process(orderbook: OrderBook, token_list: TokenList) {
    let token_pairs = get_token_pairs(token_list);
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
        )
        .unwrap();
        submit_solution(best_match).unwrap();
    }
}

fn get_token_pairs(token_list: TokenList) -> Vec<(Address, Address)> {
    let tokens: Vec<Address> = token_list.tokens.read().clone().into_iter().collect();
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
