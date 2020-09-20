use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;
use web3::types::Address;

#[derive(Debug, Clone)]
pub struct TokenList {
    pub tokens: Arc<RwLock<HashSet<Address>>>,
}

impl TokenList {
    pub fn new() -> Self {
        TokenList {
            tokens: Arc::new(RwLock::new(HashSet::new())),
        }
    }
    pub async fn add_token(&self, token: Address) {
        self.tokens.write().insert(token);
    }
    pub async fn remove_token(&self, token: Address) {
        self.tokens.write().remove(&token);
    }
}
