//! ## Storage Stripe: ordered fallback across volume tiers
//!
//! Maps the CoCapn `Stripe` pattern to storage. Each volume maps to a `TierProfile`
//! that describes its storage role. The stripe is ordered by preference:
//! Hot → Bulk → Archive, with each tier describing where to fall back when full.

use crate::tiering::StorageTier;
// DeviceTier is used in the From<StorageTier> impl for future compatibility
// with CoCapn's compute striping system.
#[allow(unused_imports)]
use crate::internal::cocapn_core::DeviceTier;
use std::collections::HashMap;
use uuid::Uuid;

/// A tier profile for a single volume in the fleet stripe.
#[derive(Debug, Clone)]
pub struct TierProfile {
    pub volume_id: Uuid,
    pub volume_name: String,
    pub storage_tier: StorageTier,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub healthy: bool,
    /// Volumes to fall back to when this volume is full/unhealthy.
    pub escalation_targets: Vec<Uuid>,
}

/// Events emitted by the storage stripe during rebalancing.
#[derive(Debug, Clone)]
pub enum StorageStripeEvent {
    /// A volume was added to the stripe.
    VolumeAdded(Uuid, StorageTier),
    /// A volume failed (full, unmounted, degraded).
    VolumeFailed { volume_id: Uuid, reason: String },
    /// Workload was rebalanced from one volume to another.
    Rebalanced {
        from: Uuid,
        to: Uuid,
        reason: String,
    },
    /// All volumes in a tier are unhealthy — emergency degradation.
    Degraded { remaining_tiers: Vec<StorageTier> },
    /// Escalation from one tier to another.
    Escalated {
        from_tier: StorageTier,
        to_tier: StorageTier,
        volume_id: Uuid,
    },
}

/// An ordered set of storage tiers. Mirrors CoCapn's `Stripe` but for storage.
#[derive(Debug, Clone)]
pub struct StorageStripe {
    /// All tier profiles keyed by volume UUID.
    profiles: HashMap<Uuid, TierProfile>,
    /// Ordered tier list for fallback.
    tier_order: Vec<StorageTier>,
}

impl StorageStripe {
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            tier_order: vec![StorageTier::Hot, StorageTier::Bulk, StorageTier::Archive],
        }
    }

    /// Register a volume in the stripe.
    pub fn add_volume(
        &mut self,
        volume_id: Uuid,
        volume_name: String,
        storage_tier: StorageTier,
        free_bytes: u64,
        total_bytes: u64,
        escalation_targets: Vec<Uuid>,
    ) -> StorageStripeEvent {
        let profile = TierProfile {
            volume_id,
            volume_name: volume_name.clone(),
            storage_tier,
            free_bytes,
            total_bytes,
            healthy: true,
            escalation_targets,
        };
        self.profiles.insert(volume_id, profile);
        StorageStripeEvent::VolumeAdded(volume_id, storage_tier)
    }

    /// Mark a volume as failed (full, unmounted, degraded).
    pub fn fail_volume(&mut self, volume_id: Uuid, reason: String) -> Option<StorageStripeEvent> {
        let profile = self.profiles.get_mut(&volume_id)?;
        profile.healthy = false;

        Some(StorageStripeEvent::VolumeFailed {
            volume_id,
            reason,
        })
    }

    /// Get the fallback path: from the current tier down to the archive tier.
    pub fn fallback_path_for(&self, volume_id: Uuid) -> Vec<Uuid> {
        let profile = match self.profiles.get(&volume_id) {
            Some(p) => p,
            None => return vec![],
        };

        let current_tier = profile.storage_tier;
        let mut path = Vec::new();

        // Walk from current tier to archive, collecting healthy volumes
        let tiers: &[StorageTier] = &[
            StorageTier::Hot,
            StorageTier::Bulk,
            StorageTier::Archive,
            StorageTier::Offload,
        ];

        let start_idx = tiers.iter().position(|t| *t == current_tier).unwrap_or(0);

        for tier in &tiers[start_idx + 1..] {
            let mut tier_vols: Vec<_> = self
                .profiles
                .values()
                .filter(|p| p.storage_tier == *tier && p.healthy)
                .collect();
            tier_vols.sort_by(|a, b| b.free_bytes.cmp(&a.free_bytes));
            for p in tier_vols {
                path.push(p.volume_id);
            }
        }

        path
    }

    /// Check if any volumes need escalation and return events.
    pub fn check_escalations(&self) -> Vec<StorageStripeEvent> {
        let mut events = Vec::new();

        for profile in self.profiles.values() {
            if !profile.healthy {
                // Find the next healthy tier
                let current_tier = profile.storage_tier;
                let tier_chain = [StorageTier::Hot, StorageTier::Bulk, StorageTier::Archive];
                let idx = tier_chain
                    .iter()
                    .position(|t| *t == current_tier)
                    .unwrap_or(0);

                // Direct escalation targets
                for target_id in &profile.escalation_targets {
                    if let Some(target) = self.profiles.get(target_id) {
                        if target.healthy {
                            events.push(StorageStripeEvent::Escalated {
                                from_tier: current_tier,
                                to_tier: target.storage_tier,
                                volume_id: target.volume_id,
                            });
                            break; // escalate to the first valid target
                        }
                    }
                }

                // If no explicit target, fall back to the next tier
                if !profile.escalation_targets.is_empty() {
                    continue;
                }

                for next_tier in &tier_chain[idx + 1..] {
                    let next_profiles: Vec<_> = self
                        .profiles
                        .values()
                        .filter(|p| p.storage_tier == *next_tier && p.healthy)
                        .collect();
                    if let Some(t) = next_profiles.first() {
                        events.push(StorageStripeEvent::Escalated {
                            from_tier: current_tier,
                            to_tier: *next_tier,
                            volume_id: t.volume_id,
                        });
                        break;
                    }
                }
            }
        }

        events
    }

    /// Get all healthy volumes for a tier.
    pub fn healthy_volumes_in_tier(&self, tier: StorageTier) -> Vec<&TierProfile> {
        self.profiles
            .values()
            .filter(|p| p.storage_tier == tier && p.healthy)
            .collect()
    }

    /// Get the current tier for a volume.
    pub fn tier_for_volume(&self, volume_id: Uuid) -> Option<StorageTier> {
        self.profiles.get(&volume_id).map(|p| p.storage_tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripe_with_volumes() -> StorageStripe {
        let mut s = StorageStripe::new();
        s.add_volume(
            Uuid::new_v4(),
            "NVMe SSD".into(),
            StorageTier::Hot,
            500_000_000_000,
            1_000_000_000_000,
            vec![],
        );
        s.add_volume(
            Uuid::new_v4(),
            "NAS".into(),
            StorageTier::Bulk,
            8_000_000_000_000,
            10_000_000_000_000,
            vec![],
        );
        s.add_volume(
            Uuid::new_v4(),
            "Backblaze B2".into(),
            StorageTier::Archive,
            50_000_000_000_000,
            100_000_000_000_000,
            vec![],
        );
        s
    }

    #[test]
    fn fallback_path_bulk_to_archive() {
        let s = stripe_with_volumes();
        let bulk_id = s
            .profiles
            .values()
            .find(|p| p.storage_tier == StorageTier::Bulk)
            .unwrap()
            .volume_id;

        let path = s.fallback_path_for(bulk_id);
        assert!(!path.is_empty());

        let archive_id = s
            .profiles
            .values()
            .find(|p| p.storage_tier == StorageTier::Archive)
            .unwrap()
            .volume_id;
        assert_eq!(path[0], archive_id);
    }

    #[test]
    fn escalation_on_failure() {
        let mut s = stripe_with_volumes();
        let hot_id = s
            .profiles
            .values()
            .find(|p| p.storage_tier == StorageTier::Hot)
            .unwrap()
            .volume_id;

        s.fail_volume(hot_id, "95% full".into());
        let events = s.check_escalations();
        assert!(!events.is_empty());

        match &events[0] {
            StorageStripeEvent::Escalated {
                from_tier,
                to_tier,
                ..
            } => {
                assert_eq!(*from_tier, StorageTier::Hot);
                assert_eq!(*to_tier, StorageTier::Bulk);
            }
            other => panic!("Expected Escalated event, got {:?}", other),
        }
    }
}
