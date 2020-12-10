use contracts::{
    ERC20Mintable, GPv2AllowListAuthentication, GPv2Settlement, UniswapV2Factory, UniswapV2Router02,
};
use ethcontract::prelude::{Account, Address, Http, PrivateKey, Web3, U256};
use model::{DomainSeparator, OrderCreationBuilder, OrderKind};
use orderbook::orderbook::OrderBook;
use secp256k1::SecretKey;
use serde_json::json;
use std::{str::FromStr, sync::Arc};
use web3::signing::SecretKeyRef;

const ONE_HUNDRED: u128 = 100_000_000_000_000_000_000;

#[tokio::test]
async fn test_with_ganache() {
    let http = Http::new("http://localhost:8545").expect("transport failure");

    let web3 = Web3::new(http);
    let accounts: Vec<Address> = web3.eth().accounts().await.expect("get accounts failed");
    let solver = Account::Local(accounts[0], None);
    let trader_a_key = PrivateKey::from_hex_str(
        "0000000000000000000000000000000000000000000000000000000000000001",
    )
    .expect("Cannot derive key");
    let trader_a = Account::Offline(trader_a_key, None);
    println!("trader_a: {}", trader_a.address());
    let trader_b_key = PrivateKey::from_hex_str(
        "0000000000000000000000000000000000000000000000000000000000000002",
    )
    .expect("Cannot derive key");
    let trader_b = Account::Offline(trader_b_key, None);
    println!("trader_b: {}", trader_b.address());

    let deploy_mintable_token = || async {
        ERC20Mintable::builder(&web3)
            .gas(8_000_000u32.into())
            .deploy()
            .await
            .expect("MintableERC20 deployment failed")
    };

    macro_rules! tx {
        ($acc:ident, $call:expr) => {{
            const NAME: &str = stringify!($call);
            $call
                .from($acc.clone())
                .gas(8_000_000u32.into())
                .send()
                .await
                .expect(&format!("{} failed", NAME))
        }};
    }

    let uniswap_factory = UniswapV2Factory::deployed(&web3)
        .await
        .expect("Failed to load deployed UniswapFactory");
    let uniswap_router = UniswapV2Router02::deployed(&web3)
        .await
        .expect("Failed to load deployed UniswapFactory");
    let gp_settlement = GPv2Settlement::deployed(&web3)
        .await
        .expect("Failed to load deployed GPv2Settlement");
    let gp_allowance = gp_settlement
        .allowance_manager()
        .call()
        .await
        .expect("Couldn't get allowance manager address");

    let token_a = deploy_mintable_token().await;
    tx!(solver, token_a.mint(solver.address(), ONE_HUNDRED.into()));
    tx!(solver, token_a.mint(trader_a.address(), ONE_HUNDRED.into()));

    let token_b = deploy_mintable_token().await;
    tx!(solver, token_b.mint(solver.address(), ONE_HUNDRED.into()));
    tx!(solver, token_b.mint(trader_b.address(), ONE_HUNDRED.into()));

    // Create and fund Uniswap pool
    tx!(
        solver,
        uniswap_factory.create_pair(token_a.address(), token_b.address())
    );
    tx!(
        solver,
        token_a.approve(uniswap_router.address(), ONE_HUNDRED.into())
    );
    tx!(
        solver,
        token_b.approve(uniswap_router.address(), ONE_HUNDRED.into())
    );
    tx!(
        solver,
        uniswap_router.add_liquidity(
            token_a.address(),
            token_b.address(),
            ONE_HUNDRED.into(),
            ONE_HUNDRED.into(),
            0_u64.into(),
            0_u64.into(),
            solver.address(),
            U256::max_value(),
        )
    );

    // Send traders some gas money to approve GPv2

    // Approve Gpv2
    tx!(trader_a, token_a.approve(gp_allowance, 100u64.into()));
    tx!(trader_b, token_b.approve(gp_allowance, 100u64.into()));

    // Place Order
    let domain_separator = DomainSeparator(
        gp_settlement
            .domain_separator()
            .call()
            .await
            .expect("Couldn't query domain separator"),
    );
    let api = orderbook::serve_task(Arc::new(OrderBook::new(domain_separator)));
    let client = reqwest::Client::new();

    let order_a = OrderCreationBuilder::default()
        .with_sell_token(token_a.address())
        .with_sell_amount(100.into())
        .with_buy_token(token_b.address())
        .with_buy_amount(90.into())
        .with_valid_to(u32::max_value())
        .with_kind(OrderKind::Sell)
        .sign_with(
            &domain_separator,
            SecretKeyRef::from(
                &SecretKey::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000001",
                )
                .unwrap(),
            ),
        )
        .build();
    let placement = client
        .post("http://localhost:8080/api/v1/orders/")
        .body(json!(order_a).to_string())
        .send()
        .await;
    assert_eq!(placement.unwrap().status(), 201);

    let order_b = OrderCreationBuilder::default()
        .with_sell_token(token_b.address())
        .with_sell_amount(100.into())
        .with_buy_token(token_a.address())
        .with_buy_amount(90.into())
        .with_valid_to(u32::max_value())
        .with_kind(OrderKind::Sell)
        .sign_with(
            &domain_separator,
            SecretKeyRef::from(
                &SecretKey::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000002",
                )
                .unwrap(),
            ),
        )
        .build();
    let placement = client
        .post("http://localhost:8080/api/v1/orders/")
        .body(json!(order_b).to_string())
        .send()
        .await;
    assert_eq!(placement.unwrap().status(), 201);

    // Wait
    let orderbook_api = solver::orderbook::OrderBookApi::new(
        reqwest::Url::from_str("http://localhost:8080").unwrap(),
        std::time::Duration::from_secs(10),
    );
    let mut driver = solver::driver::Driver {
        settlement_contract: gp_settlement,
        uniswap_router,
        orderbook: orderbook_api,
    };
    driver.single_run().await.unwrap();
    // Check matching
}
