use contracts::ERC20;
use ethcontract::{batch::CallBatch, errors::MethodError, H160, U256};
use futures::future::join_all;
use shared::Web3;
use std::collections::HashMap;

const MAX_BATCH_SIZE: usize = 100;

#[derive(Clone)]
/// Computes the amount of "buffer" ERC20 balance that the http solver can use
/// to offset possible rounding errors in computing the amounts in a solution.
pub struct BufferRetriever {
    web3: Web3,
    settlement_contract: H160,
}

impl BufferRetriever {
    pub fn new(web3: Web3, settlement_contract: H160) -> Self {
        Self {
            web3,
            settlement_contract,
        }
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait BufferRetrieving: Send + Sync {
    async fn get_buffers(&self, tokens: &[H160]) -> HashMap<H160, Result<U256, MethodError>>;
}

#[async_trait::async_trait]
impl BufferRetrieving for BufferRetriever {
    async fn get_buffers(&self, tokens: &[H160]) -> HashMap<H160, Result<U256, MethodError>> {
        let mut batch = CallBatch::new(self.web3.transport());

        let futures = tokens
            .iter()
            .map(|address| {
                let erc20 = ERC20::at(&self.web3, *address);
                erc20
                    .methods()
                    .balance_of(self.settlement_contract)
                    .batch_call(&mut batch)
            })
            .collect::<Vec<_>>();

        batch.execute_all(MAX_BATCH_SIZE).await;

        tokens
            .iter()
            .zip(join_all(futures).await.into_iter())
            .map(|(&address, balance)| (address, balance))
            .collect()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use contracts::GPv2Settlement;
    use hex_literal::hex;
    use model::order::BUY_ETH_ADDRESS;
    use shared::transport::create_test_transport;

    #[tokio::test]
    #[ignore]
    async fn retrieves_buffers_on_rinkeby() {
        let web3 = Web3::new(create_test_transport(
            &std::env::var("NODE_URL_RINKEBY").unwrap(),
        ));
        let settlement_contract = GPv2Settlement::deployed(&web3).await.unwrap();
        let weth = H160(hex!("c778417E063141139Fce010982780140Aa0cD5Ab"));
        let dai = H160(hex!("c7ad46e0b8a400bb3c915120d284aafba8fc4735"));
        let not_a_token = H160(hex!("badbadbadbadbadbadbadbadbadbadbadbadbadb"));

        let buffer_retriever = BufferRetriever::new(web3, settlement_contract.address());
        let buffers = buffer_retriever
            .get_buffers(&[weth, dai, BUY_ETH_ADDRESS, not_a_token])
            .await;
        println!("Buffers: {:#?}", buffers);
        assert!(buffers.get(&weth).unwrap().is_ok());
        assert!(buffers.get(&dai).unwrap().is_ok());
        assert!(buffers.get(&BUY_ETH_ADDRESS).unwrap().is_err());
        assert!(buffers.get(&not_a_token).unwrap().is_err());
    }
}
