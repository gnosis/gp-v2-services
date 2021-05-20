use ethcontract::common::DeploymentInformation;
use ethcontract_generate::{Address, Builder, TransactionHash};
use maplit::hashmap;
use std::{collections::HashMap, env, fs, path::Path, str::FromStr};

#[path = "src/paths.rs"]
mod paths;

fn main() {
    // NOTE: This is a workaround for `rerun-if-changed` directives for
    // non-existent files cause the crate's build unit to get flagged for a
    // rebuild if any files in the workspace change.
    //
    // See:
    // - https://github.com/rust-lang/cargo/issues/6003
    // - https://doc.rust-lang.org/cargo/reference/build-scripts.html#cargorerun-if-changedpath
    println!("cargo:rerun-if-changed=build.rs");

    generate_contract("ERC20", hashmap! {});
    generate_contract("ERC20Mintable", hashmap! {});
    generate_contract(
        "UniswapV2Router02",
        hashmap! {
            1 => (Address::from_str("7a250d5630B4cF539739dF2C5dAcb4c659F2488D").unwrap(), None),
            4 => (Address::from_str("7a250d5630B4cF539739dF2C5dAcb4c659F2488D").unwrap(), None),
            100 => (Address::from_str("1C232F01118CB8B424793ae03F870aa7D0ac7f77").unwrap(), None),
        },
    );
    generate_contract(
        "UniswapV2Factory",
        hashmap! {
            1 => (Address::from_str("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f").unwrap(), None),
            4 => (Address::from_str("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f").unwrap(), None),
            100 => (Address::from_str("A818b4F111Ccac7AA31D0BCc0806d64F2E0737D7").unwrap(), None),
        },
    );
    generate_contract("UniswapV2Pair", hashmap! {});
    // This is done to have a common interface for Sushiswap, Uniswap & Honeyswap
    generate_contract("IUniswapLikeRouter", hashmap! {});
    generate_contract("IUniswapLikePair", hashmap! {});
    generate_contract(
        "SushiswapV2Router02",
        hashmap! {
            1 => (Address::from_str("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F").unwrap(), None),
            4 => (Address::from_str("1b02dA8Cb0d097eB8D57A175b88c7D8b47997506").unwrap(), None),
            100 => (Address::from_str("1b02dA8Cb0d097eB8D57A175b88c7D8b47997506").unwrap(), None),
        },
    );
    generate_contract(
        "SushiswapV2Factory",
        hashmap! {
            1 => (Address::from_str("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac").unwrap(), None),
            4 => (Address::from_str("c35DADB65012eC5796536bD9864eD8773aBc74C4").unwrap(), None),
            100 => (Address::from_str("c35DADB65012eC5796536bD9864eD8773aBc74C4").unwrap(), None),
        },
    );
    generate_contract("SushiswapV2Pair", hashmap! {});
    generate_contract(
        "GPv2Settlement",
        hashmap! {
            1 => (Address::from_str("0x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf").unwrap(), Some("0x34b7f9a340e663df934fcc662b3ec5fcd7cd0c93d3c46f8ce612e94fff803909".parse().unwrap())),
            4 => (Address::from_str("0x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf").unwrap(), Some("0x52badda922fd91052e6682d125daa59dea3ce5c57add5a9d362bec2d6ccfd2b1".parse().unwrap())),
            100 => (Address::from_str("0x3328f5f2cEcAF00a2443082B657CedEAf70bfAEf").unwrap(), Some("0x95bbefbca7162435eeb71bac6960aae4d7112abce87a51ad3952d7b7af0279e3".parse().unwrap())),
        },
    );
    generate_contract("GPv2AllowListAuthentication", hashmap! {});
    generate_contract(
        "WETH9",
        hashmap! {
            // Rinkeby & Mainnet Addresses are part of the artefact
            100 => (Address::from_str("e91D153E0b41518A2Ce8Dd3D7944Fa863463a97d").unwrap(), None),
        },
    );
}

fn generate_contract(
    name: &str,
    deployment_overrides: HashMap<u32, (Address, Option<TransactionHash>)>,
) {
    let artifact = paths::contract_artifacts_dir().join(format!("{}.json", name));
    let address_file = paths::contract_address_file(name);
    let dest = env::var("OUT_DIR").unwrap();

    println!("cargo:rerun-if-changed={}", artifact.display());
    let mut builder = Builder::new(artifact)
        .with_contract_name_override(Some(name))
        .with_visibility_modifier(Some("pub"))
        .add_event_derive("serde::Deserialize")
        .add_event_derive("serde::Serialize");

    if let Ok(address) = fs::read_to_string(&address_file) {
        println!("cargo:rerun-if-changed={}", address_file.display());
        builder = builder.add_deployment_str(5777, address.trim());
    }

    for (network_id, (address, transaction_hash)) in deployment_overrides.into_iter() {
        builder = builder.add_deployment(
            network_id,
            address,
            transaction_hash.map(DeploymentInformation::TransactionHash),
        );
    }

    builder
        .generate()
        .unwrap()
        .write_to_file(Path::new(&dest).join(format!("{}.rs", name)))
        .unwrap();
}
