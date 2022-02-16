//! Submodule containing helper methods to pre-process auction data before passing it on to the solvers.

use crate::liquidity::LimitOrder;
use ethcontract::U256;
use lazy_static::lazy_static;
use num::{BigInt, BigRational};
use shared::conversions::U256Ext as _;

lazy_static! {
    static ref UNIT: BigInt = BigInt::from(1_000_000_000_000_000_000_u128);
}

/// Converts a token price from the orderbook API `/auction` endpoint to an
/// native token exchange rate.
pub fn to_native_xrate(price: U256) -> BigRational {
    // Prices returned by the API are already denominated in native token with
    // 18 decimals. This means, its value corresponds to how much native token
    // is needed in order to buy 1e18 of the priced token.
    // Thus, in order to compute an exchange rate from the priced token to the
    // native token we simply need to compute `price / 1e18`. This results in
    // an exchange rate such that `x TOKEN * xrate = y ETH`.
    price.to_big_rational() / &*UNIT
}

// vk: I would like to extend this to also check that the order has minimum age but for this we need
// access to the creation date which is a more involved change.
pub fn has_at_least_one_user_order(orders: &[LimitOrder]) -> bool {
    orders.iter().any(|order| !order.is_liquidity_order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use num::One as _;

    #[test]
    fn converts_prices_to_exchange_rates() {
        // By definition, the price of ETH is 1e18 and its xrate is 1.
        let eth_price = U256::from(1_000_000_000_000_000_000_u128);
        assert_eq!(to_native_xrate(eth_price), BigRational::one());

        // GNO is typically traded at around Ξ0.1. With the price
        // representation we use here, this would be 1e17.
        let gno_price = U256::from_f64_lossy(1e17);
        let gno_xrate = to_native_xrate(gno_price);
        assert_eq!(
            gno_xrate,
            BigRational::new(BigInt::from(1), BigInt::from(10))
        );

        // 1000 GNO is worth roughly 100 ETH
        let gno_amount = BigInt::from(1000) * &*UNIT;
        let eth_amount = gno_xrate * gno_amount;
        assert_eq!(
            eth_amount,
            BigRational::from_integer(BigInt::from(100) * &*UNIT)
        );
    }
}
