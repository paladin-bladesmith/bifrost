use std::collections::HashMap;
use std::sync::Arc;

use futures_util::stream::StreamExt;
use log::info;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::nonblocking::rpc_client::RpcClient;
use tokio::sync::RwLock;

const RPC_URL: &str = "https://api.testnet.solana.com";
const WS_RPC_URL: &str = "wss://api.testnet.solana.com/";

#[derive(Default, Debug)]
pub struct LeaderTracker {
    curr_slot: u64,
    slots_in_epoch: u64,
    curr_epoch_slot_start: u64,
    next_epoch_slot_start: u64,
    /// Slot index > leader identity
    curr_schedule: HashMap<usize, String>,
    next_schedule: HashMap<usize, String>,
}

impl LeaderTracker {
    pub async fn new() -> Self {
        let rpc_client = RpcClient::new(RPC_URL.to_string());

        let epoch_info = rpc_client
            .get_epoch_info()
            .await
            .expect("Failed to get epoch info");

        // TODO: We assume `leader_schedule_slot_offset` is equal to `epoch_info.slots_in_epoch`
        // Otherwise, it will be hard to know when schedule started in epoch
        let curr_epoch_slot_start = epoch_info.absolute_slot - epoch_info.slot_index;
        let next_epoch_slot_start = curr_epoch_slot_start + epoch_info.slots_in_epoch;

        let leader_ips = LeaderTracker::get_leaders_ips(&rpc_client).await;
        let curr_schedule = LeaderTracker::get_leader_schedule(
            &rpc_client,
            curr_epoch_slot_start,
            leader_ips.clone(),
        )
        .await;
        let next_schedule =
            LeaderTracker::get_leader_schedule(&rpc_client, next_epoch_slot_start, leader_ips)
                .await;

        Self {
            curr_slot: epoch_info.absolute_slot,
            slots_in_epoch: epoch_info.slots_in_epoch,
            curr_epoch_slot_start,
            next_epoch_slot_start,
            curr_schedule,
            next_schedule,
        }
    }

    /// Get the current and next `amount-1`` leader ips
    pub fn get_leaders(&self, amount: u8) -> Vec<String> {
        let mut leaders = vec![];
        for i in 0..amount {
            let slot = self.curr_slot + i as u64 * 4;

            let (slot_index, schedule) = if slot >= self.next_epoch_slot_start {
                (slot - self.next_epoch_slot_start, &self.next_schedule)
            } else {
                (slot - self.curr_epoch_slot_start, &self.curr_schedule)
            };

            if let Some(ip) = schedule.get(&(slot_index as usize)) {
                leaders.push(ip.clone());
            }
        }

        leaders
    }

    /// Get leader schedule for given slot
    async fn get_leader_schedule(
        rpc_client: &RpcClient,
        slot: u64,
        leader_ips: HashMap<String, String>,
    ) -> HashMap<usize, String> {
        let mut schedule: HashMap<usize, String> = HashMap::new();

        let tmp_schedule = rpc_client
            .get_leader_schedule(Some(slot))
            .await
            .expect("Failed to fetch leader schedule from RPC")
            .expect("No leader schedule for this slot");

        tmp_schedule.iter().for_each(|(val, slots)| {
            if let Some(ip) = leader_ips.get(val) {
                slots.iter().for_each(|slot| {
                    schedule.insert(*slot, ip.clone());
                });
            }
        });
        schedule
    }

    /// Get all cluster node leader IPs
    /// TODO: We don't get all leaders in schedule ips, check why
    /// TODO: We want to do period ip updates for the schedules to keep ips up to date
    /// and not try to connect to a hotswap node or something
    async fn get_leaders_ips(rpc_client: &RpcClient) -> HashMap<String, String> {
        let mut leader_ips: HashMap<String, String> = HashMap::new();

        let nodes = rpc_client
            .get_cluster_nodes()
            .await
            .expect("Failed to get cluster nodes");

        nodes.iter().for_each(|node| {
            if let Some(tpu_quic) = node.tpu_quic {
                leader_ips.insert(node.pubkey.to_string(), tpu_quic.to_string());
            }
        });

        leader_ips
    }

    /// Run the slot updates listener
    pub async fn slot_updates(leader_tracker: Arc<RwLock<Self>>) {
        let ws_client = PubsubClient::new(WS_RPC_URL).await.unwrap();

        let (mut slot_notifications, unsubscribe) = ws_client.slot_subscribe().await.unwrap();
        info!("Listening for slot updates...\n");

        while let Some(slot_info) = slot_notifications.next().await {
            let mut leader_tracker_write = leader_tracker.write().await;

            // Don't update slot to a past slot
            if slot_info.slot + 1 <= leader_tracker_write.curr_slot {
                continue;
            }

            leader_tracker_write.curr_slot = slot_info.slot + 1;

            // If slot is from next epoch, update tracker
            if slot_info.slot >= leader_tracker_write.next_epoch_slot_start {
                // curr epoch slot start
                leader_tracker_write.curr_epoch_slot_start =
                    leader_tracker_write.next_epoch_slot_start;

                // Set next epoch slot start
                leader_tracker_write.next_epoch_slot_start = leader_tracker_write
                    .next_epoch_slot_start
                    + leader_tracker_write.slots_in_epoch;

                // set curr schedule to next schedule
                leader_tracker_write.curr_schedule = std::mem::take(&mut leader_tracker_write.next_schedule);

                let next_epoch_slot_start = leader_tracker_write.next_epoch_slot_start;
                drop(leader_tracker_write);

                let leader_tracker_clone = leader_tracker.clone();
                tokio::spawn(async move {
                    let rpc_client = RpcClient::new(RPC_URL.to_string());

                    let leader_ips = LeaderTracker::get_leaders_ips(&rpc_client).await;
                    let next_schedule = LeaderTracker::get_leader_schedule(
                        &rpc_client,
                        next_epoch_slot_start,
                        leader_ips,
                    )
                    .await;
                    let mut leader_tracker = leader_tracker_clone.write().await;
                    leader_tracker.next_schedule = next_schedule;
                });
            }
        }

        // TODO: Handle closing the subscription properly
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use solana_sdk::epoch_info;
    use tokio::{runtime::Runtime, time::sleep};

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_get_rpc_leader_schedule() {
        let leader_tracker: Arc<RwLock<LeaderTracker>> =
            Arc::new(RwLock::new(LeaderTracker::new().await));

        let leader_tracker_clone = leader_tracker.clone();
        tokio::spawn(async move {
            LeaderTracker::slot_updates(leader_tracker_clone).await;
        });

        loop {
            let lt = leader_tracker.read().await;
            println!(
                "Current Slot: {},leaders: {:?}",
                lt.curr_slot,
                lt.get_leaders(2)
            );
            sleep(Duration::from_millis(400)).await;
        }
    }
}
