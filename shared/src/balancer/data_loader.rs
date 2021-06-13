use crate::balancer::event_handler::RegisteredWeightedPool;
use anyhow::{Context, Result};
use ethcontract::{H160, H256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub struct StoredRegistry {
    /// Used for O(1) access to all pool_ids for a given token
    pub pools_by_token: HashMap<H160, HashSet<H256>>,
    /// WeightedPool data for a given PoolId
    pub pools: HashMap<H256, RegisteredWeightedPool>,
}

impl StoredRegistry {
    pub fn read(reader: impl Read) -> Result<Self> {
        Ok(bincode::deserialize_from(reader)?)
    }

    pub fn write_to(&self, writer: impl Write) -> Result<()> {
        bincode::serialize_into(writer, self)?;
        Ok(())
    }

    pub fn write_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        // Write to tmp file until complete and then rename.
        let temp_path = path.as_ref().with_extension("temp");
        {
            // Create temp file to be written completely before rename
            let temp_file = File::create(&temp_path)
                .with_context(|| format!("couldn't create {}", temp_path.display()))?;

            let mut buffered_writer = BufWriter::new(temp_file);
            self.write_to(&mut buffered_writer)?;
            buffered_writer.flush()?;
        }
        // Rename the temp file to the originally specified path.
        fs::rename(temp_path, path)?;
        Ok(())
    }
}

impl TryFrom<File> for StoredRegistry {
    type Error = anyhow::Error;

    fn try_from(mut file: File) -> Result<Self> {
        let buffered_reader = BufReader::new(&mut file);
        let loaded_registry = StoredRegistry::read(buffered_reader)
            .with_context(|| format!("Failed to read file: {:?}", file))?;

        tracing::info!(
            "Successfully loaded Balancer Pool Registry with {} pools in {} bytes from file",
            loaded_registry.pools.len(),
            file.metadata()?.len(),
        );

        Ok(loaded_registry)
    }
}

impl TryFrom<&Path> for StoredRegistry {
    type Error = anyhow::Error;

    fn try_from(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
        StoredRegistry::try_from(file)
    }
}
