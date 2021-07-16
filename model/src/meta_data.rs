//! Contains the app_data file structures, which will also be stored in the data base and on ipfs

use crate::h160_hexadecimal::{self};
use anyhow::Result;
use cid::multihash::{Code, MultihashDigest};
use cid::Cid;
use primitive_types::H160;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use thiserror::Error;

#[derive(Error, Debug, Copy, Eq, PartialEq, Clone, Deserialize, Serialize, Hash)]
pub enum MetaDataKind {
    #[error("Referrer not found")]
    Referrer,
}

#[serde_as]
#[derive(Eq, PartialEq, Clone, Debug, Deserialize, Serialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct MetaData {
    pub version: String,
    pub kind: MetaDataKind,
    #[serde(with = "h160_hexadecimal")]
    pub referrer: H160,
}

#[serde_as]
#[derive(Eq, PartialEq, Clone, Debug, Deserialize, Serialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct AppData {
    pub version: String,
    pub app_code: String,
    pub meta_data: Vec<MetaData>,
}
const RAW: u64 = 0x55;
impl AppData {
    // following function calculates the cid. The cid is used by IPFS as data identifier
    pub fn cid(&self) -> Result<String> {
        let string = serde_json::to_string(self)?;
        let hash = Code::Sha2_256.digest(&string.into_bytes());
        let cid = Cid::new_v1(RAW, hash);
        Ok(cid.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn deserialization_and_back() {
        let value = json!(
        {
            "version": "1.0.0",
            "appCode": "CowSwap",
            "metaData": [{
              "version": "1.2.3",
              "kind": "Referrer",
              "referrer": "0x424a46612794dbb8000194937834250dc723ffa5",
            }]
        }
        );
        let expected = AppData {
            version: String::from("1.0.0"),
            app_code: String::from("CowSwap"),
            meta_data: vec![MetaData {
                version: String::from("1.2.3"),
                kind: MetaDataKind::Referrer,
                referrer: "0x424a46612794dbb8000194937834250dc723ffa5"
                    .parse()
                    .unwrap(),
            }],
        };
        let deserialized: AppData = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(deserialized, expected);
        let serialized = serde_json::to_value(expected).unwrap();
        assert_eq!(serialized, value);
    }
    #[test]
    fn test_cid_calculation() {
        let app_data = AppData {
            version: String::from("1.0.0"),
            app_code: String::from("CowSwap"),
            meta_data: vec![MetaData {
                version: String::from("1.2.3"),
                kind: MetaDataKind::Referrer,
                referrer: "0x424a46612794dbb8000194937834250dc723ffa5"
                    .parse()
                    .unwrap(),
            }],
        };
        // Todo: confirm that this is really the expected hash
        let expected = String::from("bafkreigltot3w7t6tzq5ke6tefwgzdiw6squ7okwebkooo6uf6i65ql354");
        assert_eq!(app_data.cid().unwrap(), expected);
    }
}
