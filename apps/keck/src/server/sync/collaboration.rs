use std::sync::Arc;

use axum::{
    extract::{ws::WebSocketUpgrade, Path},
    response::Response,
    Json,
};
use async_trait::async_trait;
use futures::FutureExt;
use jwst_rpc::{axum_socket_connector, handle_connector, RpcContextImpl, BroadcastChannels, BroadcastType};
use jwst_storage::{JwstStorage, JwstStorageResult};
use jwst_core::Workspace;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc::Sender};

use super::*;
use crate::server::redis_broadcast;

#[derive(Serialize)]
pub struct WebSocketAuthentication {
    protocol: String,
}

/// Redis-aware RPC context that integrates Redis sync with the standard RPC flow
struct RedisAwareContext {
    inner: Arc<Context>,
}

impl RedisAwareContext {
    fn new(context: Arc<Context>) -> Self {
        Self { inner: context }
    }
}

#[async_trait]
impl RpcContextImpl<'_> for RedisAwareContext {
    fn get_storage(&self) -> &JwstStorage {
        self.inner.get_storage()
    }

    fn get_channel(&self) -> &BroadcastChannels {
        self.inner.get_channel()
    }

    async fn get_workspace(&self, id: &str) -> JwstStorageResult<Workspace> {
        self.inner.get_workspace(id).await
    }

    async fn join_broadcast(
        &self,
        workspace: &mut Workspace,
        identifier: String,
        last_synced: Sender<i64>,
    ) -> broadcast::Sender<BroadcastType> {
        let id = workspace.id();
        info!("Setting up Redis-aware broadcast for workspace: {} (client: {})", id, identifier);
        
        // Get or create the broadcast channel using the standard method
        let broadcast_tx = self.inner.join_broadcast(workspace, identifier.clone(), last_synced).await;
        
        // If Redis sync is available, set up Redis-aware broadcasting
        if let Some(redis_sync) = self.inner.get_redis_sync() {
            info!("Integrating Redis sync for workspace: {}", id);
            redis_broadcast::setup_redis_broadcast(
                workspace,
                identifier,
                broadcast_tx.clone(),
                Some(redis_sync),
            ).await;
        } else {
            info!("No Redis sync available, using standard broadcast for workspace: {}", id);
        }

        broadcast_tx
    }
}

pub async fn auth_handler(Path(workspace_id): Path<String>) -> Json<WebSocketAuthentication> {
    info!("auth: {}", workspace_id);
    Json(WebSocketAuthentication {
        protocol: "AFFiNE".to_owned(),
    })
}

pub async fn upgrade_handler(
    Extension(context): Extension<Arc<Context>>,
    Path(workspace): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let identifier = nanoid!();
    info!("WebSocket upgrade request for workspace: {} (client: {})", workspace, identifier);
    
    if let Err(e) = context.create_workspace(workspace.clone()).await {
        error!("Failed to create workspace {}: {:?}", workspace, e);
    }
    
    // Use Redis-aware context for collaboration
    let redis_aware_context = Arc::new(RedisAwareContext::new(context.clone()));
    
    info!("Establishing WebSocket collaboration for workspace: {} (client: {})", workspace, identifier);
    
    ws.protocols(["AFFiNE"]).on_upgrade(move |socket| {
        handle_connector(redis_aware_context, workspace.clone(), identifier, move || {
            axum_socket_connector(socket, &workspace)
        })
        .map(|_| ())
    })
}

#[cfg(test)]
mod test {
    use std::{
        ffi::c_int,
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        string::String,
        sync::Arc,
        thread::sleep,
        time::Duration,
    };

    use jwst_core::{Block, DocStorage, Workspace};
    use jwst_logger::info;
    use jwst_rpc::{start_websocket_client_sync, BroadcastChannels, RpcContextImpl};
    use jwst_storage::{BlobStorageType, JwstStorage};
    use libc::{kill, SIGTERM};
    use rand::{thread_rng, Rng};
    use tokio::runtime::Runtime;

    struct TestContext {
        storage: Arc<JwstStorage>,
        channel: Arc<BroadcastChannels>,
    }

    impl TestContext {
        fn new(storage: Arc<JwstStorage>) -> Self {
            Self {
                storage,
                channel: Arc::default(),
            }
        }
    }

    impl RpcContextImpl<'_> for TestContext {
        fn get_storage(&self) -> &JwstStorage {
            &self.storage
        }

        fn get_channel(&self) -> &BroadcastChannels {
            &self.channel
        }
    }

    #[test]
    #[ignore = "not needed in ci"]
    fn client_collaboration_with_server() {
        if dotenvy::var("KECK_DEBUG").is_ok() {
            jwst_logger::init_logger("keck");
        }

        let server_port = thread_rng().gen_range(10000..=30000);
        let child = start_collaboration_server(server_port);

        let rt = Arc::new(Runtime::new().unwrap());
        let (workspace_id, mut workspace) = {
            let workspace_id = "1";
            let context = rt.block_on(async move {
                Arc::new(TestContext::new(Arc::new(
                    JwstStorage::new_with_migration("sqlite::memory:", BlobStorageType::DB)
                        .await
                        .expect("get storage: memory sqlite failed"),
                )))
            });
            let remote = format!("ws://localhost:{server_port}/collaboration/1");

            start_websocket_client_sync(
                rt.clone(),
                context.clone(),
                Arc::default(),
                remote,
                workspace_id.to_owned(),
            );

            (
                workspace_id.to_owned(),
                rt.block_on(async move { context.get_workspace(workspace_id).await.unwrap() }),
            )
        };

        for block_id in 0..3 {
            let block = create_block(&mut workspace, block_id.to_string(), "list".to_string());
            info!("from client, create a block:{:?}", serde_json::to_string(&block));
        }

        sleep(Duration::from_secs(1));
        info!("------------------after sync------------------");

        for block_id in 0..3 {
            let ret = get_block_from_server(workspace_id.clone(), block_id.to_string(), server_port);
            info!("get block {block_id} from server: {ret}");
            assert!(!ret.is_empty());
        }

        let space = workspace.get_space("blocks").unwrap();
        let blocks = space.get_blocks_by_flavour("list");
        let mut ids: Vec<_> = blocks.iter().map(|block| block.block_id()).collect();
        assert_eq!(ids.sort(), vec!["7", "8", "9"].sort());
        info!("blocks from local storage:");
        for block in blocks {
            info!("block: {:?}", block);
        }

        close_collaboration_server(child);
    }

    #[test]
    #[ignore = "not needed in ci"]
    fn client_collaboration_with_server_with_poor_connection() {
        let server_port = thread_rng().gen_range(30001..=65535);
        let child = start_collaboration_server(server_port);

        let rt = Runtime::new().unwrap();
        let workspace_id = String::from("1");
        let (storage, mut workspace) = rt.block_on(async {
            let storage: Arc<JwstStorage> = Arc::new(
                JwstStorage::new_with_migration("sqlite::memory:", BlobStorageType::DB)
                    .await
                    .expect("get storage: memory sqlite failed"),
            );
            let workspace = storage
                .docs()
                .get_or_create_workspace(workspace_id.clone())
                .await
                .expect("get workspace: {workspace_id} failed");
            (storage, workspace)
        });

        // simulate creating a block in offline environment
        let block = create_block(&mut workspace, "0".to_string(), "list".to_string());
        info!("from client, create a block: {:?}", block);
        info!(
            "get block 0 from server: {}",
            get_block_from_server(workspace_id.clone(), "0".to_string(), server_port)
        );
        assert!(get_block_from_server(workspace_id.clone(), "0".to_string(), server_port).is_empty());

        let rt = Arc::new(Runtime::new().unwrap());
        let (workspace_id, mut workspace) = {
            let workspace_id = "1";
            let context = Arc::new(TestContext::new(storage));
            let remote = format!("ws://localhost:{server_port}/collaboration/1");

            start_websocket_client_sync(
                rt.clone(),
                context.clone(),
                Arc::default(),
                remote,
                workspace_id.to_owned(),
            );

            (
                workspace_id.to_owned(),
                rt.block_on(async move { context.get_workspace(workspace_id).await.unwrap() }),
            )
        };

        info!("----------------start syncing from start_sync_thread()----------------");

        for block_id in 1..3 {
            let block = create_block(&mut workspace, block_id.to_string(), "list".to_string());
            info!("from client, create a block: {:?}", serde_json::to_string(&block));
        }

        let space = workspace.get_blocks().unwrap();
        let blocks = space.get_blocks_by_flavour("list");
        let mut ids: Vec<_> = blocks.iter().map(|block| block.block_id()).collect();
        assert_eq!(ids.sort(), vec!["0", "1", "2"].sort());
        info!("blocks from local storage:");
        for block in blocks {
            info!("block: {:?}", block);
        }

        sleep(Duration::from_secs(1));
        info!("------------------after sync------------------");

        for block_id in 0..3 {
            let ret = get_block_from_server(workspace_id.clone(), block_id.to_string(), server_port);
            info!("get block {block_id} from server: {}", ret);
            assert!(!ret.is_empty());
        }

        let space = workspace.get_blocks().unwrap();
        let blocks = space.get_blocks_by_flavour("list");
        let mut ids: Vec<_> = blocks.iter().map(|block| block.block_id()).collect();
        assert_eq!(ids.sort(), vec!["0", "1", "2"].sort());
        info!("blocks from local storage:");
        for block in blocks {
            info!("block: {:?}", block);
        }

        close_collaboration_server(child);
    }

    fn get_block_from_server(workspace_id: String, block_id: String, server_port: u16) -> String {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!(
                    "http://localhost:{server_port}/api/block/{}/{}",
                    workspace_id, block_id
                ))
                .send()
                .await
                .unwrap();
            resp.text().await.unwrap()
        })
    }

    fn create_block(workspace: &mut Workspace, block_id: String, block_flavour: String) -> Block {
        let mut space = workspace.get_space("blocks").unwrap();
        space.create(block_id, block_flavour).expect("failed to create block")
    }

    fn start_collaboration_server(port: u16) -> Child {
        let mut child = Command::new("cargo")
            .args(&["run", "-p", "keck"])
            .env("KECK_PORT", port.to_string())
            .env("USE_MEMORY_SQLITE", "true")
            .env("KECK_LOG", "debug")
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to run command");

        if let Some(ref mut stdout) = child.stdout {
            let reader = BufReader::new(stdout);

            for line in reader.lines() {
                let line = line.expect("Failed to read line");
                info!("{}", line);

                if line.contains("listening on 0.0.0.0:") {
                    info!("Keck server started");
                    break;
                }
            }
        }

        child
    }

    fn close_collaboration_server(child: Child) {
        unsafe { kill(child.id() as c_int, SIGTERM) };
    }
}
