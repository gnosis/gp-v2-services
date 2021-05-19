use crate::database::Database;
use crate::event_updater::EventUpdater;
use crate::orderbook::Orderbook;
use anyhow::Result;
use futures::try_join;
use shared::current_block::Maintaining;
use std::sync::Arc;

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
        try_join!(
            self.storage.run_maintenance(),
            self.event_updater.run_maintenance()
        )?;
        self.database.run_maintenance().await
    }
}
