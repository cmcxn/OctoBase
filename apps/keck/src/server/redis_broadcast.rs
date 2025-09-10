use std::sync::Arc;

use jwst_core::Workspace;
use jwst_rpc::BroadcastType;
use tokio::sync::broadcast;

use super::{redis_sync::{OperationType, RedisSync}, *};

/// Redis-aware broadcast implementation that sends messages to both local clients and other nodes via Redis
pub async fn setup_redis_broadcast(
    workspace: &Workspace,
    identifier: String,
    sender: broadcast::Sender<BroadcastType>,
    redis_sync: Option<Arc<RedisSync>>,
) {
    let workspace_id = workspace.id();

    // Subscribe to Redis for this workspace if Redis sync is available
    if let Some(redis_sync) = redis_sync.clone() {
        if let Err(e) = redis_sync.subscribe_workspace(&workspace_id, sender.clone()).await {
            error!("Failed to setup Redis subscription for workspace {}: {}", workspace_id, e);
        }
    }

    // Capture the current runtime handle for use in non-async callbacks
    let runtime_handle = tokio::runtime::Handle::current();

    // Awareness subscription with Redis publishing
    {
        let sender = sender.clone();
        let workspace_id = workspace.id();
        let redis_sync = redis_sync.clone();
        let runtime_handle = runtime_handle.clone();

        workspace
            .subscribe_awareness(move |awareness, e| {
                use jwst_codec::encode_awareness_as_message;

                let buffer = match encode_awareness_as_message(e.get_updated(awareness.get_states())) {
                    Ok(data) => data,
                    Err(e) => {
                        error!("failed to write awareness update: {}", e);
                        return;
                    }
                };

                // Publish to Redis first (if available)
                if let Some(redis_sync) = redis_sync.clone() {
                    let workspace_id = workspace_id.clone();
                    let buffer = buffer.clone();
                    runtime_handle.block_on(async move {
                        if let Err(e) = redis_sync.publish_operation(&workspace_id, OperationType::Awareness, buffer).await {
                            debug!("Failed to publish awareness to Redis: {}", e);
                        }
                    });
                }

                // Then send to local subscribers
                if sender.send(BroadcastType::BroadcastAwareness(buffer)).is_err() {
                    debug!("broadcast channel {workspace_id} has been closed")
                }
            })
            .await;
    }

    // Doc content subscription with Redis publishing
    {
        let sender = sender.clone();
        let workspace_id = workspace.id();
        let redis_sync = redis_sync.clone();
        let runtime_handle = runtime_handle.clone();

        workspace.subscribe_doc(move |update, history| {
            use jwst_codec::encode_update_as_message;
            use super::utils::encode_update_with_guid;

            debug!(
                "workspace {} changed: {}bytes, {} histories",
                workspace_id,
                update.len(),
                history.len()
            );

            match encode_update_with_guid(update, workspace_id.clone())
                .and_then(|update_with_guid| encode_update_as_message(update.to_vec()).map(|u| (update_with_guid, u)))
            {
                Ok((broadcast_update, sendable_update)) => {
                    // Publish to Redis first (if available)
                    if let Some(redis_sync) = redis_sync.clone() {
                        let workspace_id = workspace_id.clone();
                        let broadcast_update_clone = broadcast_update.clone();
                        let sendable_update_clone = sendable_update.clone();
                        
                        runtime_handle.block_on(async move {
                            // Publish raw content
                            if let Err(e) = redis_sync.publish_operation(&workspace_id, OperationType::RawContent, broadcast_update_clone).await {
                                debug!("Failed to publish raw content to Redis: {}", e);
                            }
                            
                            // Publish sendable content
                            if let Err(e) = redis_sync.publish_operation(&workspace_id, OperationType::Content, sendable_update_clone).await {
                                debug!("Failed to publish content to Redis: {}", e);
                            }
                        });
                    }

                    // Send to local subscribers
                    if sender
                        .send(BroadcastType::BroadcastRawContent(broadcast_update))
                        .is_err()
                    {
                        debug!("broadcast channel {workspace_id} has been closed")
                    }

                    if sender.send(BroadcastType::BroadcastContent(sendable_update)).is_err() {
                        debug!("broadcast channel {workspace_id} has been closed")
                    }
                }
                Err(e) => {
                    debug!("failed to encode update: {}", e);
                }
            }
        });
    }

    // Cleanup task
    let workspace_id = workspace.id();
    let redis_sync_cleanup = redis_sync.clone();
    tokio::spawn(async move {
        let mut rx = sender.subscribe();
        loop {
            tokio::select! {
                Ok(msg) = rx.recv() => {
                    match msg {
                        BroadcastType::CloseUser(user) if user == identifier => break,
                        BroadcastType::CloseAll => break,
                        _ => {}
                    }
                },
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    let count = sender.receiver_count();
                    if count < 1 {
                        break;
                    }
                }
            }
        }
        
        // Cleanup Redis subscription
        if let Some(redis_sync) = redis_sync_cleanup {
            redis_sync.unsubscribe_workspace(&workspace_id).await;
        }
        
        debug!("broadcast channel {workspace_id} has been closed");
    });
}