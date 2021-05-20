use crate::database::Database;
use crate::event_updater::EventUpdater;
use crate::orderbook::Orderbook;
use anyhow::Result;
use futures::future::join_all;
use shared::current_block::Maintaining;
use std::sync::Arc;

/// Collects all service components requiring maintenance on each new block
pub struct ServiceMaintenance {
    storage: Arc<Orderbook>,
    database: Database,
    event_updater: EventUpdater,
}

impl ServiceMaintenance {
    pub fn new(storage: Arc<Orderbook>, database: Database, event_updater: EventUpdater) -> Self {
        ServiceMaintenance {
            storage,
            database,
            event_updater,
        }
    }
}

#[async_trait::async_trait]
impl Maintaining for ServiceMaintenance {
    async fn run_maintenance(&self) -> Result<()> {
        for result in join_all(vec![
            self.storage.run_maintenance(),
            self.event_updater.run_maintenance(),
            self.database.run_maintenance(),
        ])
        .await
        {
            if let Err(err) = result {
                tracing::error!("failed with: {}", err);
            }
        }
        Ok(())
    }
}
