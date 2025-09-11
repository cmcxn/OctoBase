#[cfg(feature = "api")]
mod blobs;
#[cfg(feature = "api")]
mod blocks;
mod doc;

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use axum::Router;
#[cfg(feature = "api")]
use axum::{
    extract::{Json, Path},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, head, post},
};
use doc::doc_apis;
use futures::StreamExt;
use jwst_codec::encode_update_as_message;
use jwst_rpc::{broadcast::subscribe, decode_update_with_guid, BroadcastChannels, BroadcastType, RpcContextImpl};
use jwst_storage::{BlobStorageType, JwstStorage, JwstStorageResult};
use redis::AsyncCommands;
use tokio::sync::{
    broadcast::{channel as broadcast, Sender as BroadcastSender},
    Mutex, RwLock,
};

use super::*;

#[derive(Deserialize)]
#[cfg_attr(feature = "api", derive(utoipa::IntoParams))]
pub struct Pagination {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    usize::MAX
}

#[derive(Serialize)]
pub struct PageData<T> {
    total: usize,
    data: T,
}

pub struct Context {
    channel: BroadcastChannels,
    storage: JwstStorage,
    webhook: Arc<std::sync::RwLock<String>>,
    redis: Option<redis::Client>,
    ignore_updates: Arc<Mutex<HashSet<Vec<u8>>>>,
    redis_workspaces: RwLock<HashSet<String>>,
}

impl Context {
    pub async fn new(storage: Option<JwstStorage>) -> Self {
        let blob_storage_type = BlobStorageType::DB;

        let storage = if let Some(storage) = storage {
            info!("use external storage instance: {}", storage.database());
            Ok(storage)
        } else if dotenvy::var("USE_MEMORY_SQLITE").is_ok() {
            info!("use memory sqlite database");
            JwstStorage::new_with_migration("sqlite::memory:", blob_storage_type).await
        } else if let Ok(database_url) = dotenvy::var("DATABASE_URL") {
            info!("use external database: {}", database_url);
            JwstStorage::new_with_migration(&database_url, blob_storage_type).await
        } else {
            info!("use sqlite database: jwst.db");
            JwstStorage::new_with_sqlite("jwst", blob_storage_type).await
        }
        .expect("Cannot create database");

        Context {
            channel: RwLock::new(HashMap::new()),
            storage,
            webhook: Arc::new(std::sync::RwLock::new(
                dotenvy::var("HOOK_ENDPOINT").unwrap_or_default(),
            )),
            redis: dotenvy::var("REDIS_URL")
                .ok()
                .and_then(|url| redis::Client::open(url).ok()),
            ignore_updates: Arc::new(Mutex::new(HashSet::new())),
            redis_workspaces: RwLock::new(HashSet::new()),
        }
    }

    fn register_webhook(&self, workspace: Workspace) -> Workspace {
        #[cfg(feature = "api")]
        if workspace.subscribe_count() == 0 {
            use blocks::BlockHistory;

            let client = reqwest::Client::new();
            let rt = tokio::runtime::Handle::current();
            let webhook = self.webhook.clone();
            let ws_id = workspace.id();
            workspace.subscribe_doc(move |_, history| {
                if history.is_empty() {
                    return;
                }
                let webhook = webhook.read().unwrap();
                if webhook.is_empty() {
                    return;
                }
                // release the lock before move webhook
                let webhook = webhook.clone();
                rt.block_on(async {
                    debug!("send {} histories to webhook {}", history.len(), webhook);
                    let resp = client
                        .post(webhook)
                        .json(
                            &history
                                .iter()
                                .map(|h| (ws_id.as_str(), h).into())
                                .collect::<Vec<BlockHistory>>(),
                        )
                        .send()
                        .await
                        .unwrap();
                    if !resp.status().is_success() {
                        error!("failed to send webhook: {}", resp.status());
                    }
                });
            });
        }
        workspace
    }

    pub fn set_webhook(&self, endpoint: String) {
        let mut write_guard = self.webhook.write().unwrap();
        *write_guard = endpoint;
    }

    async fn spawn_redis(&self, id: String, broadcast_tx: BroadcastSender<BroadcastType>, workspace: Workspace) {
        {
            let mut guard = self.redis_workspaces.write().await;
            if !guard.insert(id.clone()) {
                return;
            }
        }

        let Some(client) = &self.redis else {
            return;
        };
        let client_pub = client.clone();
        let client_sub = client.clone();
        let channel = format!("keck:{id}");
        let ignore = self.ignore_updates.clone();
        let tx_pub = broadcast_tx.clone();
        let channel_pub = channel.clone();

        // publisher task
        tokio::spawn(async move {
            let mut conn = match client_pub.get_async_connection().await {
                Ok(c) => c,
                Err(e) => {
                    error!("redis publisher connect error: {:?}", e);
                    return;
                }
            };
            let mut rx = tx_pub.subscribe();
            while let Ok(msg) = rx.recv().await {
                match msg {
                    BroadcastType::BroadcastRawContent(update) => {
                        if ignore.lock().await.remove(&update) {
                            continue;
                        }
                        let mut payload = Vec::with_capacity(1 + update.len());
                        payload.push(0);
                        payload.extend_from_slice(&update);
                        let _: Result<(), _> = conn.publish(channel_pub.clone(), payload).await;
                    }
                    BroadcastType::BroadcastAwareness(data) => {
                        let mut payload = Vec::with_capacity(1 + data.len());
                        payload.push(1);
                        payload.extend_from_slice(&data);
                        let _: Result<(), _> = conn.publish(channel_pub.clone(), payload).await;
                    }
                    _ => {}
                }
            }
        });

        // subscriber task
        let ignore = self.ignore_updates.clone();
        let channel_sub = channel;
        let tx_sub = broadcast_tx.clone();
        tokio::spawn(async move {
            let mut conn = match client_sub.get_async_connection().await {
                Ok(c) => c,
                Err(e) => {
                    error!("redis subscriber connect error: {:?}", e);
                    return;
                }
            };
            let mut pubsub = conn.into_pubsub();
            if pubsub.subscribe(channel_sub.clone()).await.is_err() {
                error!("redis subscribe failed");
                return;
            }
            let mut stream = pubsub.on_message();
            while let Some(msg) = stream.next().await {
                let payload: Vec<u8> = match msg.get_payload() {
                    Ok(p) => p,
                    Err(e) => {
                        error!("redis payload error: {:?}", e);
                        continue;
                    }
                };
                if payload.is_empty() {
                    continue;
                }
                match payload[0] {
                    0 => {
                        let data = payload[1..].to_vec();
                        ignore.lock().await.insert(data.clone());
                        if let Ok((_, update)) = decode_update_with_guid(&data) {
                            let mut ws = workspace.clone();
                            let update_vec = update.to_vec();
                            let _ = tokio::task::spawn_blocking(move || {
                                ws.sync_messages(vec![update_vec]);
                            })
                            .await;
                            if let Ok(msg) = encode_update_as_message(update.to_vec()) {
                                let _ = tx_sub.send(BroadcastType::BroadcastContent(msg));
                            }
                            let _ = tx_sub.send(BroadcastType::BroadcastRawContent(data));
                        }
                    }
                    1 => {
                        let data = payload[1..].to_vec();
                        let _ = tx_sub.send(BroadcastType::BroadcastAwareness(data));
                    }
                    _ => {}
                }
            }
        });
    }

    pub async fn get_workspace<S>(&self, workspace_id: S) -> JwstStorageResult<Workspace>
    where
        S: AsRef<str>,
    {
        self.storage
            .get_workspace(workspace_id)
            .await
            .map(|w| self.register_webhook(w))
    }

    pub async fn init_workspace<S>(&self, workspace_id: S, data: Vec<u8>) -> JwstStorageResult
    where
        S: AsRef<str>,
    {
        self.storage.init_workspace(workspace_id, data).await
    }

    pub async fn export_workspace<S>(&self, workspace_id: S) -> JwstStorageResult<Vec<u8>>
    where
        S: AsRef<str>,
    {
        self.storage.export_workspace(workspace_id).await
    }

    pub async fn create_workspace<S>(&self, workspace_id: S) -> JwstStorageResult<Workspace>
    where
        S: AsRef<str>,
    {
        self.storage
            .create_workspace(workspace_id)
            .await
            .map(|w| self.register_webhook(w))
    }
}

#[async_trait]
impl RpcContextImpl<'_> for Context {
    fn get_storage(&self) -> &JwstStorage {
        &self.storage
    }

    fn get_channel(&self) -> &BroadcastChannels {
        &self.channel
    }

    async fn join_broadcast(
        &self,
        workspace: &mut Workspace,
        identifier: String,
        last_synced: tokio::sync::mpsc::Sender<i64>,
    ) -> BroadcastSender<BroadcastType> {
        let id = workspace.id();
        let broadcast_tx = {
            let mut channels = self.channel.write().await;
            match channels.entry(id.clone()) {
                Entry::Occupied(tx) => tx.get().clone(),
                Entry::Vacant(v) => {
                    let (tx, _) = broadcast(10240);
                    v.insert(tx.clone());
                    tx.clone()
                }
            }
        };

        subscribe(workspace, identifier.clone(), broadcast_tx.clone()).await;
        self.save_update(&id, identifier, broadcast_tx.subscribe(), last_synced)
            .await;

        if self.redis.is_some() {
            self.spawn_redis(id, broadcast_tx.clone(), workspace.clone()).await;
        }

        broadcast_tx
    }
}

pub fn api_handler(router: Router) -> Router {
    #[cfg(feature = "api")]
    {
        router.nest("/api", blobs::blobs_apis(blocks::blocks_apis(Router::new())))
    }
    #[cfg(not(feature = "api"))]
    {
        router
    }
}
