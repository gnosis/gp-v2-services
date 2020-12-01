use primitive_types::U256;

// TODO: Use this to verify that a settlement uniswap interaction works with the current state of
// the pool.
#[derive(Debug, Default)]
pub struct Uniswap {
    reserve_a: U256,
    reserve_b: U256,
}

impl Uniswap {
    // https://github.com/Uniswap/uniswap-v2-periphery/blob/4123f93278b60bcf617130629c69d4016f9e7584/contracts/libraries/UniswapV2Library.sol#L42
    fn amount_out(amount_in: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
        if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return None;
        }
        let amount_in_with_fee = amount_in.checked_mul(997.into())?;
        let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
        let denominator = reserve_in
            .checked_mul(1000.into())?
            .checked_add(amount_in_with_fee)?;
        numerator.checked_div(denominator)
    }

    // https://github.com/Uniswap/uniswap-v2-periphery/blob/4123f93278b60bcf617130629c69d4016f9e7584/contracts/libraries/UniswapV2Library.sol#L53
    fn amount_in(amount_out: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
        if amount_out.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return None;
        }
        let numerator = reserve_in
            .checked_mul(amount_out)?
            .checked_mul(1000.into())?;
        let denominator = reserve_out
            .checked_sub(amount_out)?
            .checked_mul(997.into())?;
        numerator.checked_div(denominator)?.checked_add(1.into())
    }

    fn reverse(&self) -> Self {
        Uniswap {
            reserve_a: self.reserve_b,
            reserve_b: self.reserve_a,
        }
    }

    // returns bought amount_b
    fn sell_a(&self, amount_a: U256) -> Option<U256> {
        Self::amount_out(amount_a, self.reserve_a, self.reserve_b)
    }

    // returns bought amount_a
    fn sell_b(&self, amount_b: U256) -> Option<U256> {
        Self::amount_out(amount_b, self.reserve_b, self.reserve_a)
    }

    // returns minimum amount_a that must be sold
    fn buy_a(&self, amount_a: U256) -> Option<U256> {
        Self::amount_in(amount_a, self.reserve_b, self.reserve_a)
    }

    // returns minimum amount_b that must be sold
    fn buy_b(&self, amount_b: U256) -> Option<U256> {
        Self::amount_in(amount_b, self.reserve_a, self.reserve_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sell() {
        let uniswap = Uniswap {
            reserve_a: 10000.into(),
            reserve_b: 100000.into(),
        };
        assert_eq!(uniswap.sell_a(10.into()), Some(99.into()));
        assert_eq!(uniswap.sell_b(100.into()), Some(9.into()));
    }

    #[test]
    fn buy() {
        let uniswap = Uniswap {
            reserve_a: 10000.into(),
            reserve_b: 100000.into(),
        };
        assert_eq!(uniswap.buy_a(10.into()), Some(101.into()));
        assert_eq!(uniswap.buy_b(100.into()), Some(11.into()));
    }
}
