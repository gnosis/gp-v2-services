use crate::models::solution::Solution;
use anyhow::Result;

pub fn submit_solution(solution: Solution) -> Result<()> {
    println!("{:?}", solution.sell_orders_token0);
    Ok(())
}
