use ethcontract::U256;

lazy_static::lazy_static! {
    // Estimates from multivariate linear regression here:
    // https://docs.google.com/spreadsheets/d/13UeUQ9DA4bHlcy9-i8d4nSLlCxSfjcXpTelvXYzyJzQ/edit?usp=sharing
    pub static ref GAS_PER_ORDER: U256 = U256::from(66_315);
    pub static ref GAS_PER_UNISWAP: U256 = U256::from(94_696);

    // Taken from a sample of two swaps
    // https://etherscan.io/tx/0x72d234d35fd169ef497ba0a1dc23258c96f278fb688d375d135eb012e5311009
    // https://etherscan.io/tx/0x1c345a6da1edb2bba953685a4cf85f6a0d967ac751f8c5b518578c5fd20a7c96
    pub static ref GAS_PER_BALANCER_SWAP: U256 = U256::from(120_000);
}
