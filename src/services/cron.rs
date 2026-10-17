#![allow(dead_code)]

use crate::core::context::Context;
use crate::services::api::models::agent::status::DatabaseStatus;
use crate::utils::common::vec_to_option_json;
use crate::utils::redis_client;
use crate::utils::task_manager::cron::check_and_update_cron;
use crate::utils::task_manager::tasks::{SCHEDULE_KEY, remove_task};
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{error, info};

const BACKUP_TASK_PREFIX: &str = "periodic.backup_";

pub fn backup_task_name(generated_id: &str) -> String {
    format!("{BACKUP_TASK_PREFIX}{generated_id}")
}

pub fn stale_backup_tasks(scheduled_keys: &[String], live_ids: &HashSet<&str>) -> Vec<String> {
    scheduled_keys
        .iter()
        .filter_map(|key| key.strip_prefix("redbeat:"))
        .filter(|name| {
            name.strip_prefix(BACKUP_TASK_PREFIX)
                .is_some_and(|id| !live_ids.contains(id))
        })
        .map(str::to_string)
        .collect()
}

pub struct CronService {
    ctx: Arc<Context>,
    conn: MultiplexedConnection,
}

impl CronService {
    pub async fn new(ctx: Arc<Context>) -> Self {
        let conn = redis_client::redis_connection().await;
        CronService { ctx, conn }
    }

    pub async fn sync(&mut self, database: &DatabaseStatus) -> Result<bool, String> {
        let generated_id = database.generated_id.as_str();
        let dbms = database.dbms.as_str();
        let task_name = backup_task_name(generated_id);
        let args = vec![generated_id.to_string(), dbms.to_string()];
        let storages: Option<Value> = vec_to_option_json(database.storages.clone());
        let encrypt: bool = database.encrypt;
        let metadata = json!({
            "storages": storages,
            "encrypt": encrypt
        });

        check_and_update_cron(
            &mut self.conn,
            database.data.backup.cron.clone(),
            args,
            "tasks.database.periodic_backup",
            task_name,
            Option::from(metadata),
        )
        .await;

        Ok(true)
    }

    pub async fn prune(&mut self, live_ids: &HashSet<&str>) {
        let keys: Vec<String> = match self.conn.zrange(SCHEDULE_KEY, 0, -1).await {
            Ok(keys) => keys,
            Err(e) => {
                error!("Failed to list scheduled tasks: {:?}", e);
                return;
            }
        };

        for task_name in stale_backup_tasks(&keys, live_ids) {
            match remove_task(&mut self.conn, &task_name).await {
                Ok(()) => info!(
                    "Task {} removed: database no longer returned by dashboard",
                    task_name
                ),
                Err(e) => error!("Failed to remove task {}: {:?}", task_name, e),
            }
        }
    }
}
