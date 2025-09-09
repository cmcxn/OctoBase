use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use chrono::Utc;
use futures::StreamExt;
use jwst_rpc::BroadcastType;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMessage {
    pub node_id: String,
    pub workspace_id: String,
    pub operation_type: OperationType,
    pub data: Vec<u8>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OperationType {
    Awareness,
    Content,
    RawContent,
}

pub struct RedisSync {
    client: Client,
    connection_manager: Option<ConnectionManager>,
    node_id: String,
    subscribers: Arc<RwLock<HashMap<String, broadcast::Sender<BroadcastType>>>>,
}

impl RedisSync {
    pub async fn new(redis_url: &str) -> Result<Self> {
        let client = Client::open(redis_url)?;
        let connection_manager = ConnectionManager::new(client.clone()).await.ok();
        
        let node_id = dotenvy::var("NODE_ID").unwrap_or_else(|_| nanoid!());
        info!("Redis sync initialized with node_id: {}", node_id);

        Ok(Self {
            client,
            connection_manager,
            node_id,
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub async fn is_connected(&self) -> bool {
        self.connection_manager.is_some()
    }

    pub async fn publish_operation(
        &self,
        workspace_id: &str,
        operation_type: OperationType,
        data: Vec<u8>,
    ) -> Result<()> {
        if let Some(mut conn) = self.connection_manager.clone() {
            let message = SyncMessage {
                node_id: self.node_id.clone(),
                workspace_id: workspace_id.to_string(),
                operation_type,
                data,
                timestamp: Utc::now().timestamp(),
            };

            let channel = format!("keck:sync:{}", workspace_id);
            let serialized = serde_json::to_string(&message)?;
            
            conn.publish::<_, _, ()>(&channel, serialized).await?;
            debug!("Published sync message to Redis channel: {}", channel);
        }
        Ok(())
    }

    pub async fn subscribe_workspace(
        &self,
        workspace_id: &str,
        sender: broadcast::Sender<BroadcastType>,
    ) -> Result<()> {
        self.subscribers
            .write()
            .await
            .insert(workspace_id.to_string(), sender);

        if self.connection_manager.is_some() {
            let channel = format!("keck:sync:{}", workspace_id);
            let subscribers = self.subscribers.clone();
            let node_id = self.node_id.clone();
            let client = self.client.clone();
            
            tokio::spawn(async move {
                let conn = match client.get_async_connection().await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("Failed to get Redis connection for subscription: {}", e);
                        return;
                    }
                };

                let mut pubsub = conn.into_pubsub();
                if let Err(e) = pubsub.subscribe(&channel).await {
                    error!("Failed to subscribe to Redis channel {}: {}", channel, e);
                    return;
                }

                loop {
                    match pubsub.on_message().next().await {
                        Some(msg) => {
                            if let Ok(payload) = msg.get_payload::<String>() {
                                if let Ok(sync_message) = serde_json::from_str::<SyncMessage>(&payload) {
                                    // Skip messages from the same node to avoid loops
                                    if sync_message.node_id == node_id {
                                        continue;
                                    }

                                    let subscribers_read = subscribers.read().await;
                                    if let Some(sender) = subscribers_read.get(&sync_message.workspace_id) {
                                        let broadcast_type = match sync_message.operation_type {
                                            OperationType::Awareness => BroadcastType::BroadcastAwareness(sync_message.data),
                                            OperationType::Content => BroadcastType::BroadcastContent(sync_message.data),
                                            OperationType::RawContent => BroadcastType::BroadcastRawContent(sync_message.data),
                                        };

                                        if let Err(e) = sender.send(broadcast_type) {
                                            debug!("Failed to broadcast Redis sync message: {}", e);
                                        } else {
                                            debug!("Broadcast Redis sync message from node {}", sync_message.node_id);
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            debug!("Redis pubsub connection closed for channel: {}", channel);
                            break;
                        }
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn unsubscribe_workspace(&self, workspace_id: &str) {
        self.subscribers.write().await.remove(workspace_id);
    }
}