use crate::current_block::Maintaining;
use anyhow::Result;
use futures::future::join_all;
use std::sync::Arc;

/// Collects all service components requiring maintenance on each new block
pub struct ServiceMaintenance {
    pub maintainers: Vec<Arc<dyn Maintaining>>,
}

#[async_trait::async_trait]
impl Maintaining for ServiceMaintenance {
    async fn run_maintenance(&self) -> Result<()> {
        for result in join_all(self.maintainers.iter().map(|m| m.run_maintenance())).await {
            if let Err(err) = result {
                tracing::error!("failed with: {}", err);
            }
        }
        Ok(())
    }
}
