use ethcontract::H160;
use model::{
    ratio_as_decimal,
    u256_decimal::{self, DecimalU256},
};
use num::BigRational;
use primitive_types::U256;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::collections::HashMap;

#[derive(Debug, Default, Serialize)]
pub struct BatchAuctionModel {
    pub tokens: HashMap<H160, TokenInfoModel>,
    pub orders: HashMap<usize, OrderModel>,
    pub amms: HashMap<usize, AmmModel>,
    pub metadata: Option<MetadataModel>,
}

#[derive(Debug, Serialize)]
pub struct OrderModel {
    pub sell_token: H160,
    pub buy_token: H160,
    #[serde(with = "u256_decimal")]
    pub sell_amount: U256,
    #[serde(with = "u256_decimal")]
    pub buy_amount: U256,
    pub allow_partial_fill: bool,
    pub is_sell_order: bool,
    pub fee: FeeModel,
    pub cost: CostModel,
}

#[derive(Debug, Serialize)]
pub struct AmmModel {
    #[serde(flatten)]
    pub parameters: AmmParameters,
    #[serde(with = "ratio_as_decimal")]
    pub fee: BigRational,
    pub cost: CostModel,
    pub mandatory: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
pub enum AmmParameters {
    ConstantProduct(ConstantProductPoolParameters),
    WeightedProduct(WeightedProductPoolParameters),
}

#[serde_as]
#[derive(Debug, Serialize)]
pub struct ConstantProductPoolParameters {
    #[serde_as(as = "HashMap<_, DecimalU256>")]
    pub reserves: HashMap<H160, U256>,
}

#[derive(Debug, Serialize)]
pub struct PoolTokenData {
    #[serde(with = "u256_decimal")]
    pub balance: U256,
    #[serde(with = "ratio_as_decimal")]
    pub weight: BigRational,
}

#[derive(Debug, Serialize)]
pub struct WeightedProductPoolParameters {
    pub reserves: HashMap<H160, PoolTokenData>,
}

#[derive(Debug, Serialize)]
pub struct TokenInfoModel {
    pub decimals: Option<u8>,
    pub external_price: Option<f64>,
    pub normalize_priority: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct CostModel {
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    pub token: H160,
}

#[derive(Debug, Serialize)]
pub struct FeeModel {
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    pub token: H160,
}

#[derive(Debug, Deserialize)]
pub struct SettledBatchAuctionModel {
    pub orders: HashMap<usize, ExecutedOrderModel>,
    pub amms: HashMap<usize, UpdatedAmmModel>,
    pub ref_token: H160,
    pub prices: HashMap<H160, Price>,
}

impl SettledBatchAuctionModel {
    pub fn has_execution_plan(&self) -> bool {
        self.amms
            .values()
            .into_iter()
            .all(|u| u.exec_plan.is_some())
    }
}

#[derive(Debug, Deserialize)]
pub struct Price(#[serde(with = "serde_with::rust::display_fromstr")] pub f64);

#[derive(Debug, Serialize)]
pub struct MetadataModel {
    pub environment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExecutedOrderModel {
    #[serde(with = "u256_decimal")]
    pub exec_sell_amount: U256,
    #[serde(with = "u256_decimal")]
    pub exec_buy_amount: U256,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdatedAmmModel {
    pub sell_token: H160,
    pub buy_token: H160,
    pub exec_sell_amount: U256,
    pub exec_buy_amount: U256,
    pub exec_plan: Option<ExecutionPlanCoordinatesModel>,
}

impl UpdatedAmmModel {
    /// Returns true there is at least one non-zero update.
    pub fn is_non_trivial(&self) -> bool {
        self.exec_buy_amount
            .max(self.exec_sell_amount)
            .gt(&U256::zero())
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExecutionPlanCoordinatesModel {
    pub sequence: u32,
    pub position: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::hashmap;
    use serde_json::json;

    #[test]
    fn updated_amm_model_is_non_trivial() {
        assert!(!UpdatedAmmModel {
            ..Default::default()
        }
        .is_non_trivial());

        assert!(UpdatedAmmModel {
            exec_sell_amount: U256::one(),
            ..Default::default()
        }
        .is_non_trivial());

        assert!(UpdatedAmmModel {
            exec_buy_amount: U256::one(),
            ..Default::default()
        }
        .is_non_trivial());
    }

    #[test]
    fn model_serialization() {
        let buy_token = H160::from_low_u64_be(1337);
        let sell_token = H160::from_low_u64_be(43110);
        let order_model = OrderModel {
            sell_token,
            buy_token,
            sell_amount: U256::from(1),
            buy_amount: U256::from(2),
            allow_partial_fill: false,
            is_sell_order: true,
            fee: FeeModel {
                amount: U256::from(2),
                token: sell_token,
            },
            cost: CostModel {
                amount: U256::from(1),
                token: buy_token,
            },
        };
        let constant_product_pool_model = AmmModel {
            parameters: AmmParameters::ConstantProduct(ConstantProductPoolParameters {
                reserves: hashmap! {
                    buy_token => U256::from(100),
                    sell_token => U256::from(200),
                },
            }),
            fee: BigRational::new(3.into(), 1000.into()),
            cost: CostModel {
                amount: U256::from(3),
                token: buy_token,
            },
            mandatory: false,
        };
        let weighted_product_pool_model = AmmModel {
            parameters: AmmParameters::WeightedProduct(WeightedProductPoolParameters {
                reserves: hashmap! {
                    sell_token => PoolTokenData {
                        balance: U256::from(808),
                        weight: BigRational::new(2.into(), 10.into()),
                    },
                    buy_token => PoolTokenData {
                        balance: U256::from(64),
                        weight: BigRational::new(8.into(), 10.into()),
                    }
                },
            }),
            fee: BigRational::new(2.into(), 1000.into()),
            cost: CostModel {
                amount: U256::from(2),
                token: buy_token,
            },
            mandatory: true,
        };
        let model = BatchAuctionModel {
            tokens: hashmap! {
                buy_token => TokenInfoModel {
                    decimals: Some(6),
                    external_price: Some(1.2),
                    normalize_priority: Some(1),
                },
                sell_token => TokenInfoModel {
                    decimals: Some(18),
                    external_price: Some(2345.0),
                    normalize_priority: Some(0),
                }
            },
            orders: hashmap! { 0 => order_model },
            amms: hashmap! { 0 => constant_product_pool_model, 1 => weighted_product_pool_model },
            metadata: Some(MetadataModel {
                environment: Some(String::from("Such Meta")),
            }),
        };

        let result = serde_json::to_value(&model).unwrap();

        let expected = json!({
          "tokens": {
            "0x0000000000000000000000000000000000000539": {
              "decimals": 6,
              "external_price": 1.2,
              "normalize_priority": 1
            },
            "0x000000000000000000000000000000000000a866": {
              "decimals": 18,
              "external_price": 2345.0,
              "normalize_priority": 0
            }
          },
          "orders": {
            "0": {
              "sell_token": "0x000000000000000000000000000000000000a866",
              "buy_token": "0x0000000000000000000000000000000000000539",
              "sell_amount": "1",
              "buy_amount": "2",
              "allow_partial_fill": false,
              "is_sell_order": true,
              "fee": {
                "amount": "2",
                "token": "0x000000000000000000000000000000000000a866"
              },
              "cost": {
                "amount": "1",
                "token": "0x0000000000000000000000000000000000000539"
              }
            }
          },
          "amms": {
            "0": {
              "kind": "ConstantProduct",
              "reserves": {
                "0x000000000000000000000000000000000000a866": "200",
                "0x0000000000000000000000000000000000000539": "100"
              },
              "fee": "0.003",
              "cost": {
                "amount": "3",
                "token": "0x0000000000000000000000000000000000000539"
              },
              "mandatory": false
            },
            "1": {
              "kind": "WeightedProduct",
              "reserves": {
                "0x000000000000000000000000000000000000a866": {
                    "balance": "808",
                    "weight": "0.2"
                },
                "0x0000000000000000000000000000000000000539": {
                    "balance": "64",
                    "weight": "0.8"
                },
              },
              "fee": "0.002",
              "cost": {
                "amount": "2",
                "token": "0x0000000000000000000000000000000000000539"
              },
              "mandatory": true
            }
          },
          "metadata": {
            "environment": "Such Meta"
          }
        });
        assert_eq!(result, expected);
    }
}
