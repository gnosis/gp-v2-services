use crate::account_balances::BalanceFetching;
use crate::fee::{EthAwareMinFeeCalculator, MinFeeCalculating};
use contracts::WETH9;
use ethcontract::{H160, U256};
use model::order::{BuyTokenDestination, Order, SellTokenSource, BUY_ETH_ADDRESS};
use shared::bad_token::BadTokenDetecting;
use shared::web3_traits::CodeFetching;
use std::sync::Arc;
use std::time::Duration;
use warp::{http::StatusCode, reply::Json};

pub trait WarpReplyConverting {
    fn to_warp_reply(&self) -> (Json, StatusCode);
}

#[derive(Debug)]
pub enum PreValidationError {
    Forbidden,
    InsufficientValidTo,
    TransferEthToContract,
    SameBuyAndSellToken,
    UnsupportedBuyTokenDestination(BuyTokenDestination),
    UnsupportedSellTokenSource(SellTokenSource),
    Other(anyhow::Error),
}

impl WarpReplyConverting for PreValidationError {
    fn to_warp_reply(&self) -> (Json, StatusCode) {
        match self {
            Self::UnsupportedBuyTokenDestination(dest) => (
                super::error("UnsupportedBuyTokenDestination", format!("Type {:?}", dest)),
                StatusCode::BAD_REQUEST,
            ),
            Self::UnsupportedSellTokenSource(src) => (
                super::error("UnsupportedSellTokenSource", format!("Type {:?}", src)),
                StatusCode::BAD_REQUEST,
            ),
            Self::Forbidden => (
                super::error("Forbidden", "Forbidden, your account is deny-listed"),
                StatusCode::FORBIDDEN,
            ),
            Self::InsufficientValidTo => (
                super::error(
                    "InsufficientValidTo",
                    "validTo is not far enough in the future",
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::TransferEthToContract => (
                super::error(
                    "TransferEthToContract",
                    "Sending Ether to smart contract wallets is currently not supported",
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::SameBuyAndSellToken => (
                super::error(
                    "SameBuyAndSellToken",
                    "Buy token is the same as the sell token.",
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::Other(_) => (super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
}

#[derive(Debug)]
pub enum PostValidationError {
    InsufficientFee,
    InsufficientFunds,
    UnsupportedToken(H160),
    WrongOwner(H160),
    ZeroAmount,
    Other(anyhow::Error),
}

impl WarpReplyConverting for PostValidationError {
    fn to_warp_reply(&self) -> (Json, StatusCode) {
        match self {
            Self::UnsupportedToken(token) => (
                super::error("UnsupportedToken", format!("Token address {}", token)),
                StatusCode::BAD_REQUEST,
            ),
            Self::WrongOwner(owner) => (
                super::error(
                    "WrongOwner",
                    format!(
                        "Address recovered from signature {} does not match from address",
                        owner
                    ),
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::InsufficientFunds => (
                super::error(
                    "InsufficientFunds",
                    "order owner must have funds worth at least x in his account",
                ),
                StatusCode::BAD_REQUEST,
            ),
            Self::InsufficientFee => (
                super::error("InsufficientFee", "Order does not include sufficient fee"),
                StatusCode::BAD_REQUEST,
            ),
            Self::ZeroAmount => (
                super::error("ZeroAmount", "Buy or sell amount is zero."),
                StatusCode::BAD_REQUEST,
            ),
            Self::Other(_) => (super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
}

pub struct PreOrderValidator {
    code_fetcher: Box<dyn CodeFetching>,
    native_token: WETH9,
    banned_users: Vec<H160>,
    min_order_validity_period: Duration,
}

#[derive(Default)]
pub struct PreOrderData {
    pub owner: H160,
    pub sell_token: H160,
    pub buy_token: H160,
    pub receiver: H160,
    pub valid_to: u32,
    pub buy_token_balance: BuyTokenDestination,
    pub sell_token_balance: SellTokenSource,
}

impl From<Order> for PreOrderData {
    fn from(order: Order) -> Self {
        Self {
            owner: order.order_meta_data.owner,
            sell_token: order.order_creation.sell_token,
            buy_token: order.order_creation.buy_token,
            receiver: order.actual_receiver(),
            valid_to: order.order_creation.valid_to,
            buy_token_balance: order.order_creation.buy_token_balance,
            sell_token_balance: order.order_creation.sell_token_balance,
        }
    }
}

impl PreOrderValidator {
    pub fn new(
        code_fetcher: Box<dyn CodeFetching>,
        native_token: WETH9,
        banned_users: Vec<H160>,
        min_order_validity_period: Duration,
    ) -> Self {
        Self {
            code_fetcher,
            native_token,
            banned_users,
            min_order_validity_period,
        }
    }

    pub async fn validate_partial_order(
        &self,
        order: PreOrderData,
    ) -> Result<(), PreValidationError> {
        if self.banned_users.contains(&order.owner) {
            return Err(PreValidationError::Forbidden);
        }
        if order.buy_token_balance != BuyTokenDestination::Erc20 {
            return Err(PreValidationError::UnsupportedBuyTokenDestination(
                order.buy_token_balance,
            ));
        }
        if !matches!(
            order.sell_token_balance,
            SellTokenSource::Erc20 | SellTokenSource::External
        ) {
            return Err(PreValidationError::UnsupportedSellTokenSource(
                order.sell_token_balance,
            ));
        }
        if order.valid_to
            < shared::time::now_in_epoch_seconds() + self.min_order_validity_period.as_secs() as u32
        {
            return Err(PreValidationError::InsufficientValidTo);
        }
        if has_same_buy_and_sell_token(&order, &self.native_token) {
            return Err(PreValidationError::SameBuyAndSellToken);
        }
        if order.buy_token == BUY_ETH_ADDRESS {
            let code_size = self
                .code_fetcher
                .code_size(order.receiver)
                .await
                .map_err(PreValidationError::Other)?;
            if code_size != 0 {
                return Err(PreValidationError::TransferEthToContract);
            }
        }
        Ok(())
    }
}

pub struct PostOrderValidator {
    fee_validator: Arc<EthAwareMinFeeCalculator>,
    pub bad_token_detector: Arc<dyn BadTokenDetecting>,
    balance_fetcher: Arc<dyn BalanceFetching>,
}

impl PostOrderValidator {
    pub fn new(
        fee_validator: Arc<EthAwareMinFeeCalculator>,
        bad_token_detector: Arc<dyn BadTokenDetecting>,
        balance_fetcher: Arc<dyn BalanceFetching>,
    ) -> Self {
        Self {
            fee_validator,
            bad_token_detector,
            balance_fetcher,
        }
    }
    pub async fn validate(
        &self,
        order: Order,
        sender: Option<H160>,
    ) -> Result<(), PostValidationError> {
        let order_creation = &order.order_creation;
        if order_creation.buy_amount.is_zero() || order_creation.sell_amount.is_zero() {
            return Err(PostValidationError::ZeroAmount);
        }
        let owner = order.order_meta_data.owner;
        if matches!(sender, Some(from) if from != owner) {
            return Err(PostValidationError::WrongOwner(owner));
        }
        if !self
            .fee_validator
            .is_valid_fee(
                order_creation.sell_token,
                order_creation.fee_amount,
                order_creation.app_data,
            )
            .await
        {
            return Err(PostValidationError::InsufficientFee);
        }
        for &token in &[order_creation.sell_token, order_creation.buy_token] {
            if !self
                .bad_token_detector
                .detect(token)
                .await
                .map_err(PostValidationError::Other)?
                .is_good()
            {
                return Err(PostValidationError::UnsupportedToken(token));
            }
        }
        let min_balance = match minimum_balance(&order) {
            Some(amount) => amount,
            // TODO - None happens when checked_add overflows - not insufficient funds...
            None => return Err(PostValidationError::InsufficientFunds),
        };
        if !self
            .balance_fetcher
            .can_transfer(
                order_creation.sell_token,
                owner,
                min_balance,
                order_creation.sell_token_balance,
            )
            .await
            .unwrap_or(false)
        {
            return Err(PostValidationError::InsufficientFunds);
        }
        Ok(())
    }
}

/// Returns true if the orders have same buy and sell tokens.
///
/// This also checks for orders selling wrapped native token for native token.
fn has_same_buy_and_sell_token(order: &PreOrderData, native_token: &WETH9) -> bool {
    order.sell_token == order.buy_token
        || (order.sell_token == native_token.address() && order.buy_token == BUY_ETH_ADDRESS)
}

// Min balance user must have in sell token for order to be accepted. None when addition overflows.
fn minimum_balance(order: &Order) -> Option<U256> {
    if order.order_creation.partially_fillable {
        Some(U256::from(1))
    } else {
        order
            .order_creation
            .sell_amount
            .checked_add(order.order_creation.fee_amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use shared::dummy_contract;
    use shared::web3_traits::MockCodeFetching;

    #[test]
    fn detects_orders_with_same_buy_and_sell_token() {
        let native_token = dummy_contract!(WETH9, [0xef; 20]);
        assert!(has_same_buy_and_sell_token(
            &PreOrderData {
                sell_token: H160([0x01; 20]),
                buy_token: H160([0x01; 20]),
                ..Default::default()
            },
            &native_token,
        ));
        assert!(has_same_buy_and_sell_token(
            &PreOrderData {
                sell_token: native_token.address(),
                buy_token: BUY_ETH_ADDRESS,
                ..Default::default()
            },
            &native_token,
        ));

        assert!(!has_same_buy_and_sell_token(
            &PreOrderData {
                sell_token: H160([0x01; 20]),
                buy_token: H160([0x02; 20]),
                ..Default::default()
            },
            &native_token,
        ));
        // Sell token set to 0xeee...eee has no special meaning, so it isn't
        // considered buying and selling the same token.
        assert!(!has_same_buy_and_sell_token(
            &PreOrderData {
                sell_token: BUY_ETH_ADDRESS,
                buy_token: native_token.address(),
                ..Default::default()
            },
            &native_token,
        ));
    }

    #[tokio::test]
    async fn validate_partial_order_err() {
        let mut code_fetcher = Box::new(MockCodeFetching::new());
        let native_token = dummy_contract!(WETH9, [0xef; 20]);
        let min_order_validity_period = Duration::from_secs(1);
        let banned_users = vec![H160::from_low_u64_be(1)];
        let legit_valid_to =
            shared::time::now_in_epoch_seconds() + min_order_validity_period.as_secs() as u32 + 2;
        code_fetcher
            .expect_code_size()
            .times(1)
            .return_once(|_| Ok(1));
        let validator = PreOrderValidator {
            code_fetcher,
            native_token,
            banned_users,
            min_order_validity_period,
        };
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        owner: H160::from_low_u64_be(1),
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "Forbidden"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        buy_token_balance: BuyTokenDestination::Internal,
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "UnsupportedBuyTokenDestination(Internal)"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        sell_token_balance: SellTokenSource::Internal,
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "UnsupportedSellTokenSource(Internal)"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        valid_to: 0,
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "InsufficientValidTo"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        valid_to: legit_valid_to,
                        buy_token: H160::from_low_u64_be(2),
                        sell_token: H160::from_low_u64_be(2),
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "SameBuyAndSellToken"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        valid_to: legit_valid_to,
                        buy_token: BUY_ETH_ADDRESS,
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "TransferEthToContract"
        );

        let mut code_fetcher = Box::new(MockCodeFetching::new());
        code_fetcher
            .expect_code_size()
            .times(1)
            .return_once(|_| Err(anyhow!("Failed to fetch Code Size!")));
        let validator = PreOrderValidator {
            code_fetcher,
            native_token: dummy_contract!(WETH9, [0xef; 20]),
            banned_users: vec![],
            min_order_validity_period: Duration::from_secs(1),
        };
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate_partial_order(PreOrderData {
                        valid_to: legit_valid_to,
                        buy_token: BUY_ETH_ADDRESS,
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            "Other(Failed to fetch Code Size!)"
        );
    }

    #[tokio::test]
    async fn validate_partial_order_ok() {
        let code_fetcher = Box::new(MockCodeFetching::new());
        let min_order_validity_period = Duration::from_secs(1);
        let validator = PreOrderValidator {
            code_fetcher,
            native_token: dummy_contract!(WETH9, [0xef; 20]),
            banned_users: vec![],
            min_order_validity_period: Duration::from_secs(1),
        };
        assert!(validator
            .validate_partial_order(PreOrderData {
                valid_to: shared::time::now_in_epoch_seconds()
                    + min_order_validity_period.as_secs() as u32
                    + 2,
                sell_token: H160::from_low_u64_be(1),
                buy_token: H160::from_low_u64_be(2),
                ..Default::default()
            })
            .await
            .is_ok());
    }
}
