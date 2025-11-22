use crate::Slot;
use solana_client::rpc_response::SlotUpdate;
use std::collections::VecDeque;

const MAX_SLOT_SKIP_DISTANCE: u64 = 48;
const RECENT_LEADER_SLOTS_CAPACITY: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotEvent {
    Start(Slot),
    End(Slot),
}

impl SlotEvent {
    pub fn slot(&self) -> Slot {
        match self {
            SlotEvent::Start(slot) | SlotEvent::End(slot) => *slot,
        }
    }

    pub fn is_start(&self) -> bool {
        matches!(self, SlotEvent::Start(_))
    }
}

#[derive(Debug)]
pub struct SlotsTracker {
    recent_events: VecDeque<SlotEvent>,
    current_slot: Slot,
}

impl SlotsTracker {
    pub fn new() -> Self {
        Self {
            recent_events: VecDeque::with_capacity(RECENT_LEADER_SLOTS_CAPACITY),
            current_slot: 0,
        }
    }

    pub fn current_slot(&self) -> Slot {
        self.current_slot
    }

    /// Records a slot update and returns the new current slot estimate if processed
    pub fn record(&mut self, slot_event: SlotUpdate) -> Option<Slot> {
        let event = match slot_event {
            SlotUpdate::FirstShredReceived { slot, .. } => SlotEvent::Start(slot),
            SlotUpdate::Completed { slot, .. } => SlotEvent::End(slot),
            _ => return None, // Ignore other event types
        };

        self.recent_events.push_back(event);

        // Trim to capacity
        if self.recent_events.len() > RECENT_LEADER_SLOTS_CAPACITY {
            let excess = self.recent_events.len() - RECENT_LEADER_SLOTS_CAPACITY;
            self.recent_events.drain(..excess);
        }

        self.current_slot = self.estimate_current_slot();
        Some(self.current_slot)
    }

    fn estimate_current_slot(&self) -> Slot {
        if self.recent_events.is_empty() {
            return self.current_slot;
        }

        let mut sorted_events: Vec<SlotEvent> = self.recent_events.iter().copied().collect();

        sorted_events.sort_unstable_by(|a, b| {
            a.slot()
                .cmp(&b.slot())
                .then_with(|| b.is_start().cmp(&a.is_start()))
        });

        // Use median to filter out outliers (validators broadcasting far-future slots)
        let max_idx = sorted_events.len() - 1;
        let median_idx = max_idx / 2;
        let median_slot = sorted_events[median_idx].slot();
        let expected_current = median_slot + (max_idx - median_idx) as u64;
        let max_reasonable = expected_current + MAX_SLOT_SKIP_DISTANCE;

        // Find the most recent reasonable slot
        let idx = sorted_events
            .iter()
            .rposition(|e| e.slot() <= max_reasonable)
            .unwrap_or(median_idx); // Fallback to median if all slots unreasonable

        let slot_event = &sorted_events[idx];
        if slot_event.is_start() {
            slot_event.slot()
        } else {
            slot_event.slot().saturating_add(1)
        }
    }
}

impl Default for SlotsTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker_from_slots(slots: Vec<Slot>) -> SlotsTracker {
        assert!(!slots.is_empty());
        let mut tracker = SlotsTracker::new();

        for slot in slots {
            tracker.recent_events.push_back(SlotEvent::Start(slot));
            tracker.recent_events.push_back(SlotEvent::End(slot));
        }

        tracker
    }

    #[test]
    fn test_estimate_with_sequential_slots() {
        let tracker = tracker_from_slots((1..=12).collect());
        assert_eq!(tracker.estimate_current_slot(), 13);
    }

    #[test]
    fn test_estimate_with_reverse_order() {
        let tracker = tracker_from_slots((1..=12).rev().collect());
        assert_eq!(tracker.estimate_current_slot(), 13);
    }

    #[test]
    fn test_record_updates_estimate() {
        let mut tracker = SlotsTracker::new();

        assert_eq!(
            tracker.record(SlotUpdate::FirstShredReceived {
                slot: 13,
                timestamp: 0
            }),
            Some(13)
        );
        assert_eq!(tracker.current_slot(), 13);

        assert_eq!(
            tracker.record(SlotUpdate::FirstShredReceived {
                slot: 14,
                timestamp: 0
            }),
            Some(14)
        );
        assert_eq!(tracker.current_slot(), 14);
    }

    #[test]
    fn test_outlier_rejection() {
        // Slot 100 is way beyond MAX_SLOT_SKIP_DISTANCE from slot 1
        let tracker = tracker_from_slots(vec![1, 100]);
        assert_eq!(tracker.estimate_current_slot(), 2); // Rejects 100 as outlier

        let tracker = tracker_from_slots(vec![1, 2, 100]);
        assert_eq!(tracker.estimate_current_slot(), 3);
    }
}
