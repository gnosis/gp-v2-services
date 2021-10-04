use crate::{account_balances::BalanceFetching, api::WarpReplyConverting, fee::MinFeeCalculating};
use contracts::WETH9;
use ethcontract::{H160, U256};
use model::order::{BuyTokenDestination, Order, SellTokenSource, BUY_ETH_ADDRESS};
use shared::{bad_token::BadTokenDetecting, web3_traits::CodeFetching};
use std::{sync::Arc, time::Duration};
use warp::{http::StatusCode, reply::Json};

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

#[derive(Debug)]
pub enum ValidationError {
    Pre(PreValidationError),
    Post(PostValidationError),
}

impl WarpReplyConverting for ValidationError {
    fn to_warp_reply(&self) -> (Json, StatusCode) {
        match self {
            ValidationError::Pre(pre) => pre.to_warp_reply(),
            ValidationError::Post(post) => post.to_warp_reply(),
        }
    }
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

pub struct OrderValidator {
    /// For Pre/Partial-Validation: performed during fee & quote phase
    /// when only part of the order data is available
    code_fetcher: Box<dyn CodeFetching>,
    native_token: WETH9,
    banned_users: Vec<H160>,
    min_order_validity_period: Duration,
    /// For Full-Validation: performed time of order placement
    fee_validator: Arc<dyn MinFeeCalculating>,
    bad_token_detector: Arc<dyn BadTokenDetecting>,
    balance_fetcher: Arc<dyn BalanceFetching>,
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

impl OrderValidator {
    pub fn new(
        code_fetcher: Box<dyn CodeFetching>,
        native_token: WETH9,
        banned_users: Vec<H160>,
        min_order_validity_period: Duration,
        fee_validator: Arc<dyn MinFeeCalculating>,
        bad_token_detector: Arc<dyn BadTokenDetecting>,
        balance_fetcher: Arc<dyn BalanceFetching>,
    ) -> Self {
        Self {
            code_fetcher,
            native_token,
            banned_users,
            min_order_validity_period,
            fee_validator,
            bad_token_detector,
            balance_fetcher,
        }
    }

    /// Partial (aka Pre-) Validation is aimed at catching malformed order data during the
    /// fee & quote phase (i.e. before the order is signed).
    /// Thus, partial validation *doesn't* verify:
    ///     - signatures
    ///     - user sell balances or fee sufficiency.
    ///
    /// Specifically, but *does* verify:
    ///     - if buy token is native asset, receiver is not a smart contract,
    ///     - the sender is not a banned user,
    ///     - the order validity is appropriate,
    ///     - buy_token is not the same as sell_token,
    ///     - buy and sell token destination and source are supported.
    async fn partial_validate(&self, order: PreOrderData) -> Result<(), PreValidationError> {
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

    /// This is the full order validation performed at the time of order placement
    /// (i.e. once all the required fields on an Order are provided). Specifically, verifying that
    ///     - buy & sell amounts are non-zero,
    ///     - order's owner matches the from field (if specified),
    ///     - fee is sufficient,
    ///     - buy & sell tokens passed "bad token" detection,
    ///     - user has sufficient (transferable) funds to execute the order.
    ///
    /// Furthermore, full order validation also calls partial_validate to ensure that
    /// other aspects of the order are not malformed.
    pub async fn validate(
        &self,
        order: Order,
        sender: Option<H160>,
    ) -> Result<(), ValidationError> {
        self.partial_validate(PreOrderData::from(order.clone()))
            .await
            .map_err(ValidationError::Pre)?;
        let order_creation = &order.order_creation;
        if order_creation.buy_amount.is_zero() || order_creation.sell_amount.is_zero() {
            return Err(ValidationError::Post(PostValidationError::ZeroAmount));
        }
        let owner = order.order_meta_data.owner;
        if matches!(sender, Some(from) if from != owner) {
            return Err(ValidationError::Post(PostValidationError::WrongOwner(
                owner,
            )));
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
            return Err(ValidationError::Post(PostValidationError::InsufficientFee));
        }
        for &token in &[order_creation.sell_token, order_creation.buy_token] {
            if !self
                .bad_token_detector
                .detect(token)
                .await
                .map_err(|err| ValidationError::Post(PostValidationError::Other(err)))?
                .is_good()
            {
                return Err(ValidationError::Post(
                    PostValidationError::UnsupportedToken(token),
                ));
            }
        }
        let min_balance = match minimum_balance(&order) {
            Some(amount) => amount,
            // TODO - None happens when checked_add overflows - not insufficient funds...
            //  This error should be changed to SellAmountOverflow.
            None => {
                return Err(ValidationError::Post(
                    PostValidationError::InsufficientFunds,
                ))
            }
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
            return Err(ValidationError::Post(
                PostValidationError::InsufficientFunds,
            ));
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

/// Min balance user must have in sell token for order to be accepted. None when addition overflows.
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
    use crate::{account_balances::MockBalanceFetching, fee::MockMinFeeCalculating};
    use anyhow::anyhow;
    use model::order::OrderCreation;
    use shared::{
        bad_token::{MockBadTokenDetecting, TokenQuality},
        dummy_contract,
        web3_traits::MockCodeFetching,
    };

    #[test]
    fn minimum_balance_() {
        let partially_fillable_order = Order {
            order_creation: OrderCreation {
                partially_fillable: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            minimum_balance(&partially_fillable_order),
            Some(U256::from(1))
        );
        let order = Order {
            order_creation: OrderCreation {
                sell_amount: U256::MAX,
                fee_amount: U256::from(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(minimum_balance(&order), None);
        let order = Order {
            order_creation: OrderCreation {
                sell_amount: U256::from(1),
                fee_amount: U256::from(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(minimum_balance(&order), Some(U256::from(2)));
    }

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
    async fn pre_validate_err() {
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
        let validator = OrderValidator::new(
            code_fetcher,
            native_token,
            banned_users,
            min_order_validity_period,
            Arc::new(MockMinFeeCalculating::new()),
            Arc::new(MockBadTokenDetecting::new()),
            Arc::new(MockBalanceFetching::new()),
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .partial_validate(PreOrderData {
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
                    .partial_validate(PreOrderData {
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
                    .partial_validate(PreOrderData {
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
                    .partial_validate(PreOrderData {
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
                    .partial_validate(PreOrderData {
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
                    .partial_validate(PreOrderData {
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
        let validator = OrderValidator::new(
            code_fetcher,
            dummy_contract!(WETH9, [0xef; 20]),
            vec![],
            Duration::from_secs(1),
            Arc::new(MockMinFeeCalculating::new()),
            Arc::new(MockBadTokenDetecting::new()),
            Arc::new(MockBalanceFetching::new()),
        );
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .partial_validate(PreOrderData {
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
    async fn pre_validate_ok() {
        let min_order_validity_period = Duration::from_secs(1);
        let validator = OrderValidator::new(
            Box::new(MockCodeFetching::new()),
            dummy_contract!(WETH9, [0xef; 20]),
            vec![],
            Duration::from_secs(1),
            Arc::new(MockMinFeeCalculating::new()),
            Arc::new(MockBadTokenDetecting::new()),
            Arc::new(MockBalanceFetching::new()),
        );
        assert!(validator
            .partial_validate(PreOrderData {
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

    #[tokio::test]
    async fn post_validate_err() {
        let mut fee_calculator = MockMinFeeCalculating::new();
        let mut bad_token_detector = MockBadTokenDetecting::new();
        let mut balance_fetcher = MockBalanceFetching::new();
        fee_calculator
            .expect_is_valid_fee()
            .times(1)
            .returning(|_, _, _| false);
        fee_calculator
            .expect_is_valid_fee()
            .returning(|_, _, _| true);
        bad_token_detector
            .expect_detect()
            .times(1)
            .returning(|_| Err(anyhow!("failed to detect token")));
        bad_token_detector.expect_detect().times(1).returning(|_| {
            Ok(TokenQuality::Bad {
                reason: "iz Sh%tCoin".to_string(),
            })
        });
        bad_token_detector
            .expect_detect()
            .returning(|_| Ok(TokenQuality::Good));
        balance_fetcher
            .expect_can_transfer()
            .returning(|_, _, _, _| Ok(false));
        let validator = OrderValidator::new(
            Box::new(MockCodeFetching::new()),
            dummy_contract!(WETH9, [0xef; 20]),
            vec![],
            Duration::from_secs(1),
            Arc::new(fee_calculator),
            Arc::new(bad_token_detector),
            Arc::new(balance_fetcher),
        );
        let mut order = Order {
            order_creation: OrderCreation {
                valid_to: shared::time::now_in_epoch_seconds() + 2,
                sell_token: H160::from_low_u64_be(1),
                buy_token: H160::from_low_u64_be(2),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            format!(
                "{:?}",
                validator.validate(order.clone(), None).await.unwrap_err()
            ),
            "Post(ZeroAmount)"
        );
        order.order_creation.buy_amount = U256::from(1);
        order.order_creation.sell_amount = U256::from(1);
        assert_eq!(
            format!(
                "{:?}",
                validator
                    .validate(order.clone(), Some(H160::from_low_u64_be(1)))
                    .await
                    .unwrap_err()
            ),
            "Post(WrongOwner(0x7e5f4552091a69125d5dfcb7b8c2659029395bdf))"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator.validate(order.clone(), None).await.unwrap_err()
            ),
            "Post(InsufficientFee)"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator.validate(order.clone(), None).await.unwrap_err()
            ),
            "Post(Other(failed to detect token))"
        );
        assert_eq!(
            format!(
                "{:?}",
                validator.validate(order.clone(), None).await.unwrap_err()
            ),
            "Post(UnsupportedToken(0x0000000000000000000000000000000000000001))"
        );
        order.order_creation.sell_amount = U256::MAX;
        order.order_creation.fee_amount = U256::from(1);
        assert_eq!(
            format!(
                "{:?}",
                validator.validate(order.clone(), None).await.unwrap_err()
            ),
            "Post(InsufficientFunds)"
        );
        order.order_creation.sell_amount = U256::from(1);
        assert_eq!(
            format!("{:?}", validator.validate(order, None).await.unwrap_err()),
            "Post(InsufficientFunds)"
        );
    }

    #[tokio::test]
    async fn post_validate_ok() {
        let mut fee_calculator = MockMinFeeCalculating::new();
        let mut bad_token_detector = MockBadTokenDetecting::new();
        let mut balance_fetcher = MockBalanceFetching::new();
        fee_calculator
            .expect_is_valid_fee()
            .returning(|_, _, _| true);
        bad_token_detector
            .expect_detect()
            .returning(|_| Ok(TokenQuality::Good));
        balance_fetcher
            .expect_can_transfer()
            .returning(|_, _, _, _| Ok(true));
        let validator = OrderValidator::new(
            Box::new(MockCodeFetching::new()),
            dummy_contract!(WETH9, [0xef; 20]),
            vec![],
            Duration::from_secs(1),
            Arc::new(fee_calculator),
            Arc::new(bad_token_detector),
            Arc::new(balance_fetcher),
        );
        let order = Order {
            order_creation: OrderCreation {
                valid_to: shared::time::now_in_epoch_seconds() + 2,
                sell_token: H160::from_low_u64_be(1),
                buy_token: H160::from_low_u64_be(2),
                buy_amount: U256::from(1),
                sell_amount: U256::from(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(validator.validate(order, None).await.is_ok());
    }
}
