use crate::{encoding::EncodedInteraction, settlement::Interaction};
use contracts::IZeroEx;
use ethcontract::Bytes;
use shared::zeroex_api::Order;

#[derive(Clone, Debug)]
pub struct ZeroExInteraction {
    pub order: Order,
    pub executed_amount: u128,
    pub zeroex: IZeroEx,
}

impl Interaction for ZeroExInteraction {
    fn encode(&self) -> Vec<EncodedInteraction> {
        let method = self.zeroex.fill_or_kill_limit_order(
            (
                self.order.maker_token,
                self.order.taker_token,
                self.order.maker_amount,
                self.order.taker_amount,
                self.order.taker_token_fee_amount,
                self.order.maker,
                self.order.taker,
                self.order.sender,
                self.order.fee_recipient,
                Bytes(self.order.pool.0),
                self.order.expiry,
                self.order.salt,
            ),
            (
                self.order.signature.signature_type,
                self.order.signature.v,
                Bytes(self.order.signature.r.0),
                Bytes(self.order.signature.s.0),
            ),
            self.executed_amount,
        );
        let calldata = method.tx.data.expect("no calldata").0;
        vec![(self.zeroex.address(), 0.into(), Bytes(calldata))]
    }
}
