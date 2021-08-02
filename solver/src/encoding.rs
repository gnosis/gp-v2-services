use ethcontract::Bytes;
use model::{
    order::{BalanceFrom, BalanceTo, OrderCreation, OrderKind},
    SigningScheme,
};
use primitive_types::{H160, U256};

pub type EncodedTrade = (
    U256,            // sellTokenIndex
    U256,            // buyTokenIndex
    H160,            // receiver
    U256,            // sellAmount
    U256,            // buyAmount
    u32,             // validTo
    Bytes<[u8; 32]>, // appData
    U256,            // feeAmount
    U256,            // flags
    U256,            // executedAmount
    Bytes<Vec<u8>>,  // signature
);

/// Creates the data which the smart contract's `decodeTrade` expects.
pub fn encode_trade(
    order: &OrderCreation,
    sell_token_index: usize,
    buy_token_index: usize,
    executed_amount: &U256,
) -> EncodedTrade {
    (
        sell_token_index.into(),
        buy_token_index.into(),
        order.receiver.unwrap_or_else(H160::zero),
        order.sell_amount,
        order.buy_amount,
        order.valid_to,
        Bytes(order.app_data),
        order.fee_amount,
        order_flags(order),
        *executed_amount,
        Bytes(order.signature.to_bytes().to_vec()),
    )
}

fn order_flags(order: &OrderCreation) -> U256 {
    let mut result = 0u8;
    result |= match order.kind {
        OrderKind::Sell => 0b0,
        OrderKind::Buy => 0b1,
    };
    result |= (order.partially_fillable as u8) << 1;
    result |= match order.sell_token_balance {
        BalanceFrom::Erc20 => 0b00,
        BalanceFrom::External => 0b10,
        BalanceFrom::Internal => 0b11,
    } << 2;
    result |= match order.buy_token_balance {
        BalanceTo::Erc20 => 0b0,
        BalanceTo::Internal => 0b1,
    } << 4;
    result |= match order.signing_scheme {
        SigningScheme::Eip712 => 0b00,
        SigningScheme::EthSign => 0b01,
    } << 5;
    result.into()
}

pub type EncodedInteraction = (
    H160,           // target
    U256,           // value
    Bytes<Vec<u8>>, // callData
);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncodedSettlement {
    pub tokens: Vec<H160>,
    pub clearing_prices: Vec<U256>,
    pub trades: Vec<EncodedTrade>,
    pub interactions: [Vec<EncodedInteraction>; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_flag_permutations() {
        for (order, flags) in &[
            (
                OrderCreation {
                    kind: OrderKind::Sell,
                    partially_fillable: false,
                    sell_token_balance: BalanceFrom::Erc20,
                    buy_token_balance: BalanceTo::Erc20,
                    signing_scheme: SigningScheme::Eip712,
                    ..Default::default()
                },
                0b0000000,
            ),
            (
                OrderCreation {
                    kind: OrderKind::Sell,
                    partially_fillable: true,
                    sell_token_balance: BalanceFrom::Erc20,
                    buy_token_balance: BalanceTo::Internal,
                    signing_scheme: SigningScheme::Eip712,
                    ..Default::default()
                },
                0b0010010,
            ),
            (
                OrderCreation {
                    kind: OrderKind::Buy,
                    partially_fillable: false,
                    sell_token_balance: BalanceFrom::External,
                    buy_token_balance: BalanceTo::Erc20,
                    signing_scheme: SigningScheme::Eip712,
                    ..Default::default()
                },
                0b0001001,
            ),
            (
                OrderCreation {
                    kind: OrderKind::Sell,
                    partially_fillable: false,
                    sell_token_balance: BalanceFrom::Internal,
                    buy_token_balance: BalanceTo::Erc20,
                    signing_scheme: SigningScheme::EthSign,
                    ..Default::default()
                },
                0b0101100,
            ),
            (
                OrderCreation {
                    kind: OrderKind::Buy,
                    partially_fillable: true,
                    sell_token_balance: BalanceFrom::Internal,
                    buy_token_balance: BalanceTo::Internal,
                    signing_scheme: SigningScheme::EthSign,
                    ..Default::default()
                },
                0b0111111,
            ),
        ] {
            assert_eq!(order_flags(order), U256::from(*flags));
        }
    }
}
