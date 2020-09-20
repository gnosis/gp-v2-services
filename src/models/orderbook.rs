use crate::models::order::Order;
use parking_lot::RwLock;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use web3::types::Address;

// consider using a binary heap instead of vec for faster inserting
pub type OrderBookHashMap = HashMap<Address, HashMap<Address, Vec<Order>>>;

#[derive(Clone, Deserialize)]
pub struct OrderBook {
    #[serde(with = "arc_rwlock_serde")]
    pub orderbook: Arc<RwLock<OrderBookHashMap>>,
}

mod arc_rwlock_serde {
    use parking_lot::RwLock;
    use serde::de::Deserializer;
    use serde::Deserialize;
    use std::sync::Arc;

    pub fn deserialize<'de, D, T>(d: D) -> Result<Arc<RwLock<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Ok(Arc::new(RwLock::new(T::deserialize(d)?)))
    }
}

impl OrderBook {
    pub fn new() -> Self {
        OrderBook {
            orderbook: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    // pub fn get(&self, element: Address) -> Option<HashMap<Address, Vec<Order>>> {
    //     return self.get(element);
    // }
}
