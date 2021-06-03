use crate::Web3;
use anyhow::Result;
use ethcontract::H160;

#[mockall::automock]
#[async_trait::async_trait]
pub trait CodeFetching: Send + Sync {
    /// Fethces the code size at the specified address.
    async fn code_size(&self, address: H160) -> Result<usize>;
}

#[async_trait::async_trait]
impl CodeFetching for Web3 {
    async fn code_size(&self, address: H160) -> Result<usize> {
        Ok(self.eth().code(address, None).await?.0.len())
    }
}
