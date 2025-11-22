use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use solana_client::nonblocking::rpc_client::RpcClient;


#[derive(Debug)]
pub struct ScheduleTracker {
    curr_epoch_slot_start: u64,
    next_epoch_slot_start: u64,
    /// Maps slot index within epoch -> validator pubkey
    curr_schedule: HashMap<usize, String>,
    next_schedule: HashMap<usize, String>,
    slots_in_epoch: u64,
}

impl ScheduleTracker {
    /// Creates a new ScheduleTracker by fetching current and next epoch schedules.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - RPC connection fails
    /// - Epoch info is invalid
    /// - Leader schedule fetch fails
    pub async fn new(rpc_client: &RpcClient) -> Result<Self> {
        let epoch_info = rpc_client
            .get_epoch_info()
            .await
            .context("Failed to fetch epoch info from RPC")?;

        // Validate epoch info
        ensure!(
            epoch_info.slots_in_epoch > 0,
            "Invalid slots_in_epoch: {}",
            epoch_info.slots_in_epoch
        );

        ensure!(
            epoch_info.slot_index < epoch_info.slots_in_epoch,
            "slot_index {} exceeds slots_in_epoch {}",
            epoch_info.slot_index,
            epoch_info.slots_in_epoch
        );

        // Calculate epoch boundaries
        let curr_epoch_slot_start = epoch_info.absolute_slot - epoch_info.slot_index;
        let next_epoch_slot_start = curr_epoch_slot_start + epoch_info.slots_in_epoch;

        // Fetch both schedules
        let curr_schedule = Self::fetch_schedule(rpc_client, curr_epoch_slot_start)
            .await
            .context("Failed to fetch current epoch schedule")?;

        let next_schedule = Self::fetch_schedule(rpc_client, next_epoch_slot_start)
            .await
            .context("Failed to fetch next epoch schedule")?;

        Ok(Self {
            curr_epoch_slot_start,
            next_epoch_slot_start,
            curr_schedule,
            next_schedule,
            slots_in_epoch: epoch_info.slots_in_epoch,
        })
    }

    /// Fetches the leader schedule for a given epoch.
    ///
    /// # Arguments
    ///
    /// * `rpc_client` - The RPC client to use
    /// * `slot` - The first slot of the epoch
    ///
    /// # Returns
    ///
    /// A HashMap mapping slot indices to validator pubkeys
    pub async fn fetch_schedule(
        rpc_client: &RpcClient,
        slot: u64,
    ) -> Result<HashMap<usize, String>> {
        let leader_schedule = rpc_client
            .get_leader_schedule(Some(slot))
            .await
            .context("RPC call to get_leader_schedule failed")?
            .context(format!("No leader schedule available for slot {}", slot))?;

        // Convert from RPC format: {pubkey: [slot_indices]}
        // to our format: {slot_index: pubkey}
        let mut schedule = HashMap::with_capacity(leader_schedule.len() * 4);

        for (pubkey, slot_indices) in leader_schedule {
            for &slot_index in &slot_indices {
                schedule.insert(slot_index, pubkey.clone());
            }
        }

        ensure!(
            !schedule.is_empty(),
            "Fetched empty schedule for slot {}",
            slot
        );

        Ok(schedule)
    }

    pub fn get_leader_for_slot_index(&self, slot_index: usize) -> Option<&str> {
        self.curr_schedule.get(&slot_index).map(|s| s.as_str())
    }

    pub fn current_epoch_slot_start(&self) -> u64 {
        self.curr_epoch_slot_start
    }

    pub fn next_epoch_slot_start(&self) -> u64 {
        self.next_epoch_slot_start
    }

    pub fn slots_in_epoch(&self) -> u64 {
        self.slots_in_epoch
    }

    /// Rotates to the next epoch and fetches the new next_schedule.
    ///
    /// # Returns
    ///
    /// Returns `true` if rotation occurred, `false` if current slot is still in current epoch.
    pub async fn maybe_rotate(
        &mut self,
        current_slot: u64,
        rpc_client: &RpcClient,
    ) -> Result<bool> {
        if current_slot < self.next_epoch_slot_start {
            return Ok(false); // Still in current epoch
        }

        // Rotate to next epoch
        self.curr_epoch_slot_start = self.next_epoch_slot_start;
        self.next_epoch_slot_start += self.slots_in_epoch;
        self.curr_schedule = std::mem::take(&mut self.next_schedule);

        // Fetch new next epoch schedule
        self.next_schedule = Self::fetch_schedule(rpc_client, self.next_epoch_slot_start)
            .await
            .context("Failed to fetch next epoch schedule after rotation")?;

        Ok(true)
    }

    /// Converts an absolute slot number to a slot index within the current epoch.
    ///
    /// Returns `None` if the slot is outside the current epoch range.
    pub fn slot_to_index(&self, slot: u64) -> Option<usize> {
        if slot < self.curr_epoch_slot_start {
            return None; // Slot is in the past
        }

        if slot >= self.next_epoch_slot_start {
            return None; // Slot is in future epoch
        }

        let index = slot - self.curr_epoch_slot_start;
        Some(index as usize)
    }

    // Expose public fields only when absolutely necessary for external access
    #[doc(hidden)]
    pub fn curr_schedule_ref(&self) -> &HashMap<usize, String> {
        &self.curr_schedule
    }

    #[doc(hidden)]
    pub fn next_schedule_mut(&mut self) -> &mut HashMap<usize, String> {
        &mut self.next_schedule
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slot_to_index() {
        let tracker = ScheduleTracker {
            curr_epoch_slot_start: 1000,
            next_epoch_slot_start: 1432,
            curr_schedule: HashMap::new(),
            next_schedule: HashMap::new(),
            slots_in_epoch: 432,
        };

        assert_eq!(tracker.slot_to_index(1000), Some(0));
        assert_eq!(tracker.slot_to_index(1001), Some(1));
        assert_eq!(tracker.slot_to_index(1431), Some(431));
        assert_eq!(tracker.slot_to_index(999), None); // Before epoch
        assert_eq!(tracker.slot_to_index(1432), None); // After epoch
    }
}
