use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::stream::StreamExt;
use log::{error, info, warn};
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::RwLock;

use crate::tpu_client::tracker::schedule_tracking::ScheduleTracker;
use crate::tpu_client::tracker::slots_tracker::SlotsTracker;

pub const RPC_URL: &str = "https://api.devnet.solana.com";
const WS_RPC_URL: &str = "wss://api.devnet.solana.com/";

/**
 * We have 3 actions that are needed in order to track leaders properly:
 * 1. Get current slot
 * 2. Get leader schedule, next_leader_schedule is moved to current on epoch change, and we fetch new next_leader_schedule
 * 3. Update leader ips per identety, because validators might change their IP, so we need to periodically update the ips in the schedule
 *
 * This will allow us to get the leader ip to send TXs to for the next slot, by:
 * 1. Get current slot
 * 2. Get identity of the leader for this current slot (or next slot)
 * 3. Get ip of the leader from the identity
 *
 * The reason we separate identities from IPs and track them separately is because the schedule is based on identities,
 * and the IPs can change anytime, this way we are not locking the schedule while doing ip updates.
**/

#[derive(Debug)]
pub struct LeaderTracker {
    pub slots_tracker: RwLock<SlotsTracker>,
    schedule_tracker: RwLock<ScheduleTracker>,
    leader_sockets: RwLock<HashMap<String, String>>,
}

impl LeaderTracker {
    pub async fn new() -> Result<Self> {
        let rpc_client = RpcClient::new(RPC_URL.to_string());

        let schedule_tracker = ScheduleTracker::new(&rpc_client)
            .await
            .context("Failed to initialize schedule tracker")?;

        Ok(Self {
            slots_tracker: RwLock::new(SlotsTracker::new()),
            schedule_tracker: RwLock::new(schedule_tracker),
            leader_sockets: RwLock::new(HashMap::new()),
        })
    }

    pub async fn get_future_leaders(&self, start: u64, end: u64) -> Vec<(String, String, u64)> {
        // Acquire all locks together for consistent view
        let slot_tracker = self.slots_tracker.read().await;
        let schedule_tracker = self.schedule_tracker.read().await;
        let leader_sockets = self.leader_sockets.read().await;

        let curr_slot = slot_tracker.current_slot();

        if curr_slot == 0 {
            return vec![];
        }

        // Validate we're in the current epoch
        if curr_slot < schedule_tracker.current_epoch_slot_start()
            || curr_slot >= schedule_tracker.next_epoch_slot_start()
        {
            warn!(
                "Current slot {} is outside epoch range [{}, {})",
                curr_slot,
                schedule_tracker.current_epoch_slot_start(),
                schedule_tracker.next_epoch_slot_start()
            );
            return vec![];
        }

        let mut leaders = Vec::new();
        let mut seen = HashSet::new();

        for i in start..end {
            let target_slot = match curr_slot.checked_add(i) {
                Some(s) => s,
                None => break, // Overflow protection
            };

            // Skip if out of current epoch range
            if target_slot >= schedule_tracker.next_epoch_slot_start() {
                break;
            }

            // Convert absolute slot to epoch-relative index
            let slot_index = match schedule_tracker.slot_to_index(target_slot) {
                Some(idx) => idx,
                None => continue,
            };

            // Get leader for this slot
            if let Some(leader_pubkey) = schedule_tracker.get_leader_for_slot_index(slot_index) {
                // Deduplicate - only add each leader once
                if !seen.insert(leader_pubkey.to_string()) {
                    continue;
                }

                match leader_sockets.get(leader_pubkey) {
                    Some(socket) => {
                        leaders.push((leader_pubkey.to_string(), socket.clone(), curr_slot));
                    }
                    None => {
                        warn!("Leader {} has no known socket address", leader_pubkey);
                    }
                }
            }
        }

        leaders
    }

    /// Get the current leader, and next leader if close to leader switch
    ///
    /// Output = Vec<(leader identity, leader socket, current slot)>
    pub async fn get_leaders(&self) -> Vec<(String, String, u64)> {
        self.get_future_leaders(0, 2).await
    }

    /// Get all cluster node leader IPs
    pub async fn update_leader_sockets(leader_tracker: Arc<LeaderTracker>) -> Result<()> {
        let rpc_client = RpcClient::new(RPC_URL.to_string());

        let nodes = rpc_client
            .get_cluster_nodes()
            .await
            .context("Failed to fetch cluster nodes")?;

        let mut new_sockets = HashMap::new();

        for node in nodes {
            if let (Some(tpu_quic), Some(gossip)) = (node.tpu_quic, node.gossip) {
                new_sockets.insert(
                    node.pubkey.to_string(),
                    format!("{}:{}", gossip.ip(), tpu_quic.port()),
                );
            }
        }

        info!("Updated sockets for {} validators", new_sockets.len());

        let mut sockets = leader_tracker.leader_sockets.write().await;
        *sockets = new_sockets; // Move instead of clone

        Ok(())
    }

    /// Run the slot updates listener
    pub async fn run(leader_tracker: Arc<LeaderTracker>) -> Result<()> {
        let ws_client = PubsubClient::new(WS_RPC_URL)
            .await
            .context("Failed to connect to WebSocket")?;

        let (mut slot_notifications, _unsubscribe) = ws_client
            .slot_updates_subscribe()
            .await
            .context("Failed to subscribe to slot updates")?;

        info!("Listening for slot updates...");

        while let Some(slot_event) = slot_notifications.next().await {
            if let Err(e) = Self::handle_slot_event(&leader_tracker, slot_event).await {
                error!("Error handling slot event: {}", e);
                // Continue processing other events
            }
        }

        Ok(())
    }

    /// Handles a single slot update event.
    async fn handle_slot_event(
        leader_tracker: &Arc<LeaderTracker>,
        slot_event: solana_client::rpc_response::SlotUpdate,
    ) -> Result<()> {
        // Record the slot event and get updated slot number
        let curr_slot = {
            let mut slot_tracker = leader_tracker.slots_tracker.write().await;
            match slot_tracker.record(slot_event) {
                Some(slot) => slot,
                None => return Ok(()), // Ignored event type
            }
        };

        // Check if we need to rotate to next epoch
        let needs_rotation = {
            let schedule_tracker = leader_tracker.schedule_tracker.read().await;
            curr_slot >= schedule_tracker.next_epoch_slot_start()
        };

        if needs_rotation {
            Self::rotate_epoch(leader_tracker, curr_slot).await?;
        }

        Ok(())
    }

    /// Rotates the schedule to the next epoch and fetches the new next_schedule.
    async fn rotate_epoch(leader_tracker: &Arc<LeaderTracker>, curr_slot: u64) -> Result<()> {
        let rpc_client = RpcClient::new(RPC_URL.to_string());

        let mut schedule_tracker = leader_tracker.schedule_tracker.write().await;

        
        info!(
            "Rotating epoch: {} -> {}",
            schedule_tracker.current_epoch_slot_start(),
            schedule_tracker.next_epoch_slot_start()
        );

        // Use the built-in rotation method
        match schedule_tracker.maybe_rotate(curr_slot, &rpc_client).await {
            Ok(true) => {
                info!("Successfully rotated to next epoch");
            }
            Ok(false) => {
                // Shouldn't happen since we checked needs_rotation, but handle it
                warn!("Rotation not needed despite check");
            }
            Err(e) => {
                error!("Failed to rotate epoch: {}", e);
                return Err(e).context("Epoch rotation failed");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    #[ignore]
    async fn test_get_rpc_leader_schedule() {
        let leader_tracker = Arc::new(
            LeaderTracker::new()
                .await
                .expect("Failed to initialize LeaderTracker"),
        );

        let leader_tracker_clone = leader_tracker.clone();
        tokio::spawn(async move {
            if let Err(e) = LeaderTracker::run(leader_tracker_clone).await {
                eprintln!("Run error: {}", e);
            }
        });

        let leader_tracker_clone = leader_tracker.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) =
                    LeaderTracker::update_leader_sockets(leader_tracker_clone.clone()).await
                {
                    eprintln!("Socket update error: {}", e);
                }
                sleep(Duration::from_secs(60)).await;
            }
        });

        loop {
            sleep(Duration::from_millis(400)).await;

            let curr_slot = {
                let lt = leader_tracker.slots_tracker.read().await;
                lt.current_slot()
            };

            if curr_slot == 0 {
                continue;
            }

            let leaders = leader_tracker.get_leaders().await;
            println!("Current Slot: {}, leaders: {:?}", curr_slot, leaders);
        }
    }
}
