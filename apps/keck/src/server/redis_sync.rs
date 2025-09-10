use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Result, Context as AnyhowContext};
use chrono::Utc;
use futures::StreamExt;
use jwst_rpc::BroadcastType;
use redis::{aio::ConnectionManager, AsyncCommands, Client, RedisResult};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tokio::time::timeout;
use url::Url;

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
        info!("Initializing Redis sync with URL: {}", Self::sanitize_url(redis_url));
        
        // Check if Redis service is available first
        let client = Client::open(redis_url)
            .with_context(|| format!("Failed to create Redis client with URL: {}", Self::sanitize_url(redis_url)))?;
        
        // Test connection with timeout
        let connection_test = timeout(Duration::from_secs(5), async {
            let mut conn = client.get_async_connection().await?;
            let _: String = redis::cmd("PING").query_async(&mut conn).await?;
            info!("Redis connection test successful");
            Ok::<(), redis::RedisError>(())
        }).await;

        let connection_manager = match connection_test {
            Ok(Ok(())) => {
                info!("Redis is available, creating connection manager");
                match ConnectionManager::new(client.clone()).await {
                    Ok(manager) => {
                        info!("Redis connection manager created successfully");
                        Some(manager)
                    }
                    Err(e) => {
                        warn!("Failed to create Redis connection manager: {}, falling back to single-node mode", e);
                        None
                    }
                }
            }
            Ok(Err(e)) => {
                warn!("Redis connection test failed: {}, falling back to single-node mode", e);
                None
            }
            Err(_) => {
                warn!("Redis connection test timed out after 5 seconds, falling back to single-node mode");
                None
            }
        };
        
        let node_id = dotenvy::var("NODE_ID").unwrap_or_else(|_| nanoid!());
        
        if connection_manager.is_some() {
            info!("Redis sync initialized successfully with node_id: {}", node_id);
        } else {
            warn!("Redis sync initialized in single-node mode with node_id: {}", node_id);
        }

        Ok(Self {
            client,
            connection_manager,
            node_id,
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Sanitize Redis URL for logging (hide password)
    fn sanitize_url(url: &str) -> String {
        if let Ok(parsed) = Url::parse(url) {
            if parsed.password().is_some() {
                format!("{}://{}:***@{}:{}/{}",
                    parsed.scheme(),
                    parsed.username(),
                    parsed.host_str().unwrap_or("localhost"),
                    parsed.port().unwrap_or(6379),
                    parsed.path().trim_start_matches('/'))
            } else {
                url.to_string()
            }
        } else {
            url.to_string()
        }
    }

    pub async fn is_connected(&self) -> bool {
        if let Some(ref manager) = self.connection_manager {
            // Test the connection with a quick ping
            match timeout(Duration::from_secs(1), async {
                let mut conn = manager.clone();
                let _: RedisResult<String> = redis::cmd("PING").query_async(&mut conn).await;
                true
            }).await {
                Ok(_) => {
                    debug!("Redis connection health check passed");
                    true
                }
                Err(_) => {
                    warn!("Redis connection health check timed out");
                    false
                }
            }
        } else {
            false
        }
    }

    pub async fn test_redis_service(redis_url: &str) -> bool {
        info!("Testing Redis service availability at: {}", Self::sanitize_url(redis_url));
        
        match timeout(Duration::from_secs(3), async {
            let client = Client::open(redis_url)?;
            let mut conn = client.get_async_connection().await?;
            let response: String = redis::cmd("PING").query_async(&mut conn).await?;
            Ok::<String, redis::RedisError>(response)
        }).await {
            Ok(Ok(response)) => {
                info!("Redis service test successful, response: {}", response);
                true
            }
            Ok(Err(e)) => {
                warn!("Redis service test failed: {}", e);
                false
            }
            Err(_) => {
                warn!("Redis service test timed out after 3 seconds");
                false
            }
        }
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
                operation_type: operation_type.clone(),
                data,
                timestamp: Utc::now().timestamp(),
            };

            let channel = format!("keck:sync:{}", workspace_id);
            let serialized = serde_json::to_string(&message)
                .with_context(|| "Failed to serialize sync message")?;
            
            debug!("Publishing {:?} operation to Redis channel: {} (node: {}, size: {} bytes)", 
                   operation_type, channel, self.node_id, serialized.len());
            
            match timeout(Duration::from_secs(2), conn.publish::<_, _, ()>(&channel, serialized)).await {
                Ok(Ok(_)) => {
                    debug!("Successfully published sync message to Redis channel: {}", channel);
                }
                Ok(Err(e)) => {
                    error!("Failed to publish to Redis channel {}: {}", channel, e);
                    return Err(e.into());
                }
                Err(_) => {
                    error!("Redis publish operation timed out for channel: {}", channel);
                    return Err(anyhow::anyhow!("Redis publish timeout"));
                }
            }
        } else {
            debug!("Redis not connected, skipping publish for workspace: {}", workspace_id);
        }
        Ok(())
    }

    pub async fn subscribe_workspace(
        &self,
        workspace_id: &str,
        sender: broadcast::Sender<BroadcastType>,
    ) -> Result<()> {
        info!("Setting up Redis subscription for workspace: {} (node: {})", workspace_id, self.node_id);
        
        self.subscribers
            .write()
            .await
            .insert(workspace_id.to_string(), sender);

        if self.connection_manager.is_some() {
            let channel = format!("keck:sync:{}", workspace_id);
            let subscribers = self.subscribers.clone();
            let node_id = self.node_id.clone();
            let client = self.client.clone();
            let workspace_id_clone = workspace_id.to_string();
            
            info!("Starting Redis subscription task for channel: {}", channel);
            
            tokio::spawn(async move {
                let conn = match timeout(Duration::from_secs(5), client.get_async_connection()).await {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(e)) => {
                        error!("Failed to get Redis connection for subscription: {}", e);
                        return;
                    }
                    Err(_) => {
                        error!("Redis connection timeout for subscription");
                        return;
                    }
                };

                let mut pubsub = conn.into_pubsub();
                if let Err(e) = timeout(Duration::from_secs(5), pubsub.subscribe(&channel)).await {
                    error!("Failed to subscribe to Redis channel {} (timeout or error): {:?}", channel, e);
                    return;
                }

                info!("Successfully subscribed to Redis channel: {}", channel);

                loop {
                    match timeout(Duration::from_secs(30), pubsub.on_message().next()).await {
                        Ok(Some(msg)) => {
                            if let Ok(payload) = msg.get_payload::<String>() {
                                debug!("Received Redis message on channel {}: {} bytes", channel, payload.len());
                                
                                match serde_json::from_str::<SyncMessage>(&payload) {
                                    Ok(sync_message) => {
                                        // Skip messages from the same node to avoid loops
                                        if sync_message.node_id == node_id {
                                            debug!("Skipping message from same node: {}", node_id);
                                            continue;
                                        }

                                        debug!("Processing sync message from node: {} for workspace: {} (type: {:?})", 
                                               sync_message.node_id, sync_message.workspace_id, sync_message.operation_type);

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
                                                debug!("Successfully broadcast Redis sync message from node {} to workspace {}", 
                                                       sync_message.node_id, sync_message.workspace_id);
                                            }
                                        } else {
                                            debug!("No local subscribers for workspace: {}", sync_message.workspace_id);
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to deserialize Redis sync message: {}", e);
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            warn!("Redis pubsub connection closed for channel: {}", channel);
                            break;
                        }
                        Err(_) => {
                            debug!("Redis pubsub timeout for channel: {}, checking connection...", channel);
                            // Connection health check - just continue for now since we can't easily check pubsub connection
                            warn!("Redis pubsub timeout for channel: {}, connection may be unstable", channel);
                            continue;
                        }
                    }
                }
                
                warn!("Redis subscription task ended for workspace: {}", workspace_id_clone);
            });
        } else {
            info!("Redis not connected, workspace {} will operate in single-node mode", workspace_id);
        }

        Ok(())
    }

    pub async fn unsubscribe_workspace(&self, workspace_id: &str) {
        info!("Unsubscribing from Redis for workspace: {} (node: {})", workspace_id, self.node_id);
        self.subscribers.write().await.remove(workspace_id);
    }
}