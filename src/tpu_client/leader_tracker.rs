//! Leader Schedule Construction Without RPC
//!
//! This module should implement Solana's leader schedule algorithm locally, computing the same
//! deterministic schedule that all validators use without calling `getLeaderSchedule` RPC.
//!
//! To build the leader schedule, we need:
//!
//! 1. **Stake Data** - Map of `Pubkey → u64` stake amounts for each validator
//!    - Fetch once per epoch via `getVoteAccounts` RPC and cache
//!    - Alternative: Parse stake account state from ledger/shreds
//!
//! 2. **Epoch Number** - The epoch for which to compute the schedule
//!    - Calculate from: current_slot / slots_per_epoch
//!
//! 3. **Slots Per Epoch** - Total slots in the epoch (e.g., 432,000 on mainnet)
//!    - Fetch once via `getEpochSchedule` RPC and cache
//!    - Or use known constant for your cluster
//!
//! Once we have this data, compute the schedule locally and cache it for the entire epoch.
//!
//! Core Algorithm: `stake_weighted_slot_leaders()`
//!
//! The Agave algorithm from `agave/ledger/src/leader_schedule.rs`:
//!
//! ```rust,ignore
//! use rand::distributions::{Distribution, WeightedIndex};
//! use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
//!
//! fn stake_weighted_slot_leaders(
//!     mut keyed_stakes: Vec<(&Pubkey, u64)>,
//!     epoch: u64,
//!     len: u64,      // slots_per_epoch
//!     repeat: u64,   // NUM_CONSECUTIVE_LEADER_SLOTS = 4
//! ) -> Vec<Pubkey> {
//!     // Step 1: Deterministic sorting
//!     sort_stakes(&mut keyed_stakes);
//!     
//!     // Step 2: Separate keys and weights
//!     let (keys, stakes): (Vec<_>, Vec<_>) = keyed_stakes.into_iter().unzip();
//!     
//!     // Step 3: Create weighted distribution
//!     let weighted_index = WeightedIndex::new(stakes).unwrap();
//!     
//!     // Step 4: Seed deterministic RNG with epoch
//!     let mut seed = [0u8; 32];
//!     seed[0..8].copy_from_slice(&epoch.to_le_bytes());
//!     let rng = &mut ChaChaRng::from_seed(seed);
//!     
//!     // Step 5: Generate schedule with leader chunks
//!     let mut current_slot_leader = Pubkey::default();
//!     (0..len)
//!         .map(|i| {
//!             if i % repeat == 0 {
//!                 current_slot_leader = keys[weighted_index.sample(rng)];
//!             }
//!             current_slot_leader
//!         })
//!         .collect()
//! }
//!
//! fn sort_stakes(stakes: &mut Vec<(&Pubkey, u64)>) {
//!     // Sort by stake DESC, then pubkey DESC for determinism
//!     stakes.sort_unstable_by(|(l_pubkey, l_stake), (r_pubkey, r_stake)| {
//!         if r_stake == l_stake {
//!             r_pubkey.cmp(l_pubkey)  // Tie-breaker
//!         } else {
//!             r_stake.cmp(l_stake)
//!         }
//!     });
//!     stakes.dedup();
//! }
//! ```
//!
//! Algorithm Breakdown
//!
//! i: Deterministic Sorting
//!
//! Validators must be sorted identically on all nodes:
//! - **Primary sort**: By stake amount (highest first)
//! - **Tie-breaker**: By pubkey (reverse lexicographic order)
//! - **Deduplication**: Remove any duplicate entries
//!
//! ```rust,ignore
//! // Example stakes before sorting:
//! [
//!     (Pubkey("AAA..."), 1000),
//!     (Pubkey("BBB..."), 1000),  // Same stake
//!     (Pubkey("CCC..."), 500),
//! ]
//!
//! // After sorting:
//! [
//!     (Pubkey("BBB..."), 1000),  // Higher pubkey wins tie
//!     (Pubkey("AAA..."), 1000),
//!     (Pubkey("CCC..."), 500),
//! ]
//! ```
//!
//! ii: Weighted Distribution
//!
//! Use `rand::distributions::WeightedIndex` to create a probability distribution:
//! - Validators with more stake get selected more frequently
//! - Probability of selection = validator_stake / total_stake
//!
//! iii: Deterministic RNG
//!
//! Seed ChaCha20 RNG with the epoch number:
//! - `seed[0..8] = epoch.to_le_bytes()`
//! - All nodes use identical seed → identical random sequence
//! - Schedule is deterministic and verifiable
//!
//! Step iv: 4-Slot Leader Chunks
//!
//! Leaders are assigned in groups of 4 consecutive slots:
//! - Slot 0-3: Leader A
//! - Slot 4-7: Leader B
//! - Slot 8-11: Leader C
//! - etc.
//!
//! Every 4th slot (when `i % 4 == 0`), sample a new leader from the weighted distribution.
//!
//! ## Implementation Example
//!
//! ```rust,ignore
//! use std::collections::HashMap;
//! use solana_sdk::pubkey::Pubkey;
//! use rand::distributions::{Distribution, WeightedIndex};
//! use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
//!
//! pub struct LeaderScheduleBuilder {
//!     stakes: HashMap<Pubkey, u64>,
//!     epoch: u64,
//!     slots_per_epoch: u64,
//! }
//!
//! impl LeaderScheduleBuilder {
//!     pub fn new(stakes: HashMap<Pubkey, u64>, epoch: u64, slots_per_epoch: u64) -> Self {
//!         Self { stakes, epoch, slots_per_epoch }
//!     }
//!     
//!     pub fn build(&self) -> Vec<Pubkey> {
//!         // Collect and sort stakes
//!         let mut keyed_stakes: Vec<_> = self.stakes.iter()
//!             .map(|(pk, stake)| (pk, *stake))
//!             .collect();
//!         
//!         keyed_stakes.sort_unstable_by(|(l_pk, l_stake), (r_pk, r_stake)| {
//!             if r_stake == l_stake {
//!                 r_pk.cmp(l_pk)
//!             } else {
//!                 r_stake.cmp(l_stake)
//!             }
//!         });
//!         keyed_stakes.dedup();
//!         
//!         // Build weighted distribution
//!         let (keys, stakes): (Vec<_>, Vec<_>) = keyed_stakes.into_iter().unzip();
//!         let weighted_index = WeightedIndex::new(stakes).unwrap();
//!         
//!         // Seed RNG with epoch
//!         let mut seed = [0u8; 32];
//!         seed[0..8].copy_from_slice(&self.epoch.to_le_bytes());
//!         let rng = &mut ChaChaRng::from_seed(seed);
//!         
//!         // Generate schedule
//!         let mut current_leader = Pubkey::default();
//!         (0..self.slots_per_epoch)
//!             .map(|i| {
//!                 if i % 4 == 0 {
//!                     current_leader = *keys[weighted_index.sample(rng)];
//!                 }
//!                 current_leader
//!             })
//!             .collect()
//!     }
//! }
//! ```
//!
//! Getting Stake Data
//!
//! - From RPC (fetch once, cache for epoch)
//!
//! ```rust,ignore
//! // Fetch stake data once per epoch
//! let vote_accounts = rpc_client.get_vote_accounts().await?;
//! let stakes: HashMap<Pubkey, u64> = vote_accounts
//!     .current
//!     .iter()
//!     .map(|account| (account.node_pubkey, account.activated_stake))
//!     .collect();
//!
//! // Build and cache schedule for entire epoch
//! let schedule = LeaderScheduleBuilder::new(stakes, epoch, slots_per_epoch).build();
//! ```
//!
//! - From Shreds (no RPC)
//!
//! Track stake account updates from shreds containing vote transactions:
//! - Parse vote state updates from shreds
//! - Track delegations and activations
//! - Maintain epoch boundary snapshots
//! - Requires replicating Bank's stake calculation logic
//!
//! From Ledger/Snapshot (no RPC)
//!
//! - Load stake accounts from ledger or snapshot
//! - Query stake account state at epoch boundaries
//! - Compute activated stake from account data
//! - Requires ledger access or snapshot parsing
//!
//! Usage Pattern
//!
//! ```rust,ignore
//! // One-time setup per epoch
//! let stakes = fetch_stakes_once(epoch).await?;
//! let builder = LeaderScheduleBuilder::new(stakes, epoch, 432_000);
//! let schedule = builder.build();  // Vec<Pubkey> with 432,000 entries
//!
//! // Look up leader for any slot (no RPC needed)
//! let slot = 123_456;
//! let slot_in_epoch = slot % 432_000;
//! let leader_pubkey = schedule[slot_in_epoch as usize];
//!
//! // Map leader to TPU address using gossip ClusterInfo
//! let tpu_address = cluster_info.lookup_contact_info(&leader_pubkey, |ci| {
//!     ci.tpu(Protocol::QUIC)
//! })?;
//! ```
//!
//!
//! Constants
//!
//! - `NUM_CONSECUTIVE_LEADER_SLOTS`: 4
//! - `SLOTS_PER_EPOCH` (mainnet): 432,000
//! - `SLOTS_PER_EPOCH` (devnet/testnet): Varies