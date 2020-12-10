use std::convert::TryInto;

use contracts::UniswapV2Router02;
use model::{OrderCreation, OrderKind};
use primitive_types::{H160, U256};

const TRADE_STRIDE: usize = 204;
const UNISWAP_INTERACTION_SIZE: usize = 260;

/// Creates the data which the smart contract's `decodeTrade` expects.
pub fn encode_trade(
    order: &OrderCreation,
    sell_token_index: u8,
    buy_token_index: u8,
    executed_amount: U256,
) -> [u8; TRADE_STRIDE] {
    let mut result = [0u8; TRADE_STRIDE];
    result[0] = sell_token_index;
    result[1] = buy_token_index;
    order.sell_amount.to_big_endian(&mut result[2..34]);
    order.buy_amount.to_big_endian(&mut result[34..66]);
    result[66..70].copy_from_slice(&order.valid_to.to_be_bytes());
    result[70..74].copy_from_slice(&order.app_data.to_be_bytes());
    order.fee_amount.to_big_endian(&mut result[74..106]);
    result[106] = encode_order_flags(order);
    executed_amount.to_big_endian(&mut result[107..139]);
    result[139] = order.signature.v;
    result[140..172].copy_from_slice(order.signature.r.as_bytes());
    result[172..204].copy_from_slice(order.signature.s.as_bytes());
    result
}

fn encode_order_flags(order: &OrderCreation) -> u8 {
    let mut result = 0u8;
    if matches!(order.order_kind, OrderKind::Buy) {
        result |= 0b00000001;
    };
    if order.partially_fillable {
        result |= 0b00000010;
    }
    result
}

fn encode_uniswap_call(
    amount_in: U256,
    amount_out_min: U256,
    token_in: H160,
    token_out: H160,
    payout_to: H160,
) -> [u8; UNISWAP_INTERACTION_SIZE] {
    let uniswap = UniswapV2Router02::at(&dummy::dummy_web3(), H160::zero());
    let method = uniswap.swap_exact_tokens_for_tokens(
        amount_in,
        amount_out_min,
        vec![token_in, token_out],
        payout_to,
        U256::MAX,
    );
    let data = method.tx.data.expect("call doesn't have calldata").0;
    data.try_into().expect("unexpected data size")
}

// To create an ethcontract instance we need to provide a web3 even though we never use it. This
// module provides a dummy transport and web3.
mod dummy {
    use jsonrpc_core::Call as RpcCall;
    use serde_json::Value;
    use web3::{api::Web3, Transport};

    #[derive(Clone, Debug)]
    pub struct DummyTransport;
    impl Transport for DummyTransport {
        type Out = futures::future::Pending<web3::Result<Value>>;
        fn prepare(&self, _method: &str, _params: Vec<Value>) -> (web3::RequestId, RpcCall) {
            unimplemented!()
        }
        fn send(&self, _id: web3::RequestId, _request: RpcCall) -> Self::Out {
            unimplemented!()
        }
    }

    pub fn dummy_web3() -> Web3<DummyTransport> {
        Web3::new(DummyTransport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::Signature;
    use primitive_types::{H160, H256};

    fn u8_as_32_bytes_be(u: u8) -> [u8; 32] {
        let mut result = [0u8; 32];
        result[31] = u;
        result
    }

    #[test]
    fn encode_trade_() {
        let order = OrderCreation {
            sell_token: H160::zero(),
            buy_token: H160::zero(),
            sell_amount: 4.into(),
            buy_amount: 5.into(),
            valid_to: 6,
            app_data: 7,
            fee_amount: 8.into(),
            order_kind: OrderKind::Buy,
            partially_fillable: true,
            signature: Signature {
                v: 9,
                r: H256::from_low_u64_be(10),
                s: H256::from_low_u64_be(11),
            },
        };
        let encoded = encode_trade(&order, 1, 2, 3.into());
        assert_eq!(encoded[0], 1);
        assert_eq!(encoded[1], 2);
        assert_eq!(encoded[2..34], u8_as_32_bytes_be(4));
        assert_eq!(encoded[34..66], u8_as_32_bytes_be(5));
        assert_eq!(encoded[66..70], [0, 0, 0, 6]);
        assert_eq!(encoded[70..74], [0, 0, 0, 7]);
        assert_eq!(encoded[74..106], u8_as_32_bytes_be(8));
        assert_eq!(encoded[106], 0b11);
        assert_eq!(encoded[107..139], u8_as_32_bytes_be(3));
        assert_eq!(encoded[139], 9);
        assert_eq!(encoded[140..172], u8_as_32_bytes_be(10));
        assert_eq!(encoded[172..204], u8_as_32_bytes_be(11));
    }

    #[test]
    fn encode_uniswap_call_() {
        let amount_in = 5;
        let amount_out_min = 6;
        let token_in = 7;
        let token_out = 8;
        let payout_to = 9;
        let encoded = encode_uniswap_call(
            U256::from(amount_in),
            U256::from(amount_out_min),
            H160::from_low_u64_be(token_in as u64),
            H160::from_low_u64_be(token_out as u64),
            H160::from_low_u64_be(payout_to as u64),
        );
        let signature = [0x38u8, 0xed, 0x17, 0x39];
        let path_offset = 160;
        let path_size = 2;
        let deadline = [0xffu8; 32];
        assert_eq!(encoded[0..4], signature);
        assert_eq!(encoded[4..36], u8_as_32_bytes_be(amount_in));
        assert_eq!(encoded[36..68], u8_as_32_bytes_be(amount_out_min));
        assert_eq!(encoded[68..100], u8_as_32_bytes_be(path_offset));
        assert_eq!(encoded[100..132], u8_as_32_bytes_be(payout_to));
        assert_eq!(encoded[132..164], deadline);
        assert_eq!(encoded[164..196], u8_as_32_bytes_be(path_size));
        assert_eq!(encoded[196..228], u8_as_32_bytes_be(token_in));
        assert_eq!(encoded[228..260], u8_as_32_bytes_be(token_out));
    }
}
