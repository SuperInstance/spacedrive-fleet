//! ## Escalation: when the deadband fires, the stripe rebalances
//!
//! This module connects deadband triggers to stripe rebalancing.
//! When a volume's storage deadband exceeds the threshold:
//!
//! 1. Deadband fires → `StorageDeadbandState::Exceeded`
//! 2. Escalation checks the stripe for the next tier
//! 3. Initiates a `StorageHandoff` from full volume → target volume
//! 4. Crossfade migrates cold files while the user continues working
//!
//! The user never notices — their new files go to the target while
//! reads on existing files are proxied (push-down principle).

use crate::{
    deadband::StorageDeadbandState,
    stripe::{StorageStripe, StorageStripeEvent},
    tiering::StorageTier,
};
use crate::internal::cocapn_core::DeadbandState;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

/// An action taken by the escalation system.
#[derive(Debug, Clone)]
pub enum EscalationAction {
    /// Begin a handoff from one volume to another.
    Migrate {
        from_volume: Uuid,
        to_volume: Uuid,
        from_tier: StorageTier,
        to_tier: StorageTier,
        total_bytes: u64,
    },
    /// Notify that a tier is fully degraded.
    Degraded {
        tier: StorageTier,
        volumes: Vec<Uuid>,
    },
    /// No action needed — all volumes healthy.
    NoOp,
}

/// Configuration for the escalation engine.
#[derive(Debug, Clone)]
pub struct EscalationConfig {
    /// How long to wait before starting a handoff after deadband fires.
    pub cooldown: Duration,
    /// Which file kinds to migrate first (cold → archive).
    pub prioritize_cold_files: bool,
    /// Minimum utilization of the *target* volume before considering it full.
    pub target_max_util_pct: f64,
    /// Whether to auto-approve migrations or require user confirmation.
    pub auto_migrate: bool,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(300), // 5 min cooldown
            prioritize_cold_files: true,
            target_max_util_pct: 85.0,
            auto_migrate: true,
        }
    }
}

/// The escalation engine. Connects deadband → stripe → handoff.
#[derive(Debug)]
pub struct EscalationEngine {
    config: EscalationConfig,
    /// Track last escalation time per volume to avoid rapid re-escalation.
    last_escalation: HashMap<uuid::Uuid, chrono::DateTime<chrono::Utc>>,
}

impl EscalationEngine {
    pub fn new(config: EscalationConfig) -> Self {
        Self {
            config,
            last_escalation: HashMap::new(),
        }
    }

    /// Process deadband results and return actions to take.
    pub fn process_deadband(
        &mut self,
        deadband_states: &[StorageDeadbandState],
        stripe: &mut StorageStripe,
    ) -> Vec<EscalationAction> {
        let now = chrono::Utc::now();
        let mut actions = Vec::new();

        for state in deadband_states {
            if state.deadband_state != DeadbandState::Exceeded {
                continue;
            }

            // Check cooldown
            if let Some(last) = self.last_escalation.get(&state.volume_id) {
                if now.signed_duration_since(*last).to_std().ok()
                    .map(|d| d < self.config.cooldown)
                    .unwrap_or(false)
                {
                    continue; // still in cooldown
                }
            }

            // Find the volume in the stripe and fail it
            let vol_tier = stripe.tier_for_volume(state.volume_id);

            // Mark as failed in the stripe
            stripe.fail_volume(
                state.volume_id,
                format!("{:.1}% full", state.utilization_pct),
            );

            // Check stripe for escalation events
            let stripe_events = stripe.check_escalations();

            for event in &stripe_events {
                match event {
                    StorageStripeEvent::Escalated {
                        from_tier,
                        to_tier,
                        volume_id: target_id,
                    } => {
                        // Find the target volume's profile for size estimate
                        let total_bytes = if let Some(_tier) = &vol_tier {
                            // Estimate: migrate files until target hits max util
                            let target_profile = stripe
                                .healthy_volumes_in_tier(*to_tier)
                                .into_iter()
                                .find(|p| p.volume_id == *target_id);

                            if let Some(t) = target_profile {
                                // How much can the target absorb?
                                let available =
                                    t.free_bytes as f64 * (1.0 - self.config.target_max_util_pct / 100.0);
                                available as u64
                            } else {
                                0
                            }
                        } else {
                            0
                        };

                        if total_bytes > 0 && self.config.auto_migrate {
                            self.last_escalation.insert(state.volume_id, now);
                            actions.push(EscalationAction::Migrate {
                                from_volume: state.volume_id,
                                to_volume: *target_id,
                                from_tier: *from_tier,
                                to_tier: *to_tier,
                                total_bytes,
                            });
                        }
                    }
                    StorageStripeEvent::Degraded { remaining_tiers: _ } => {
                        actions.push(EscalationAction::Degraded {
                            tier: vol_tier
                                .unwrap_or(StorageTier::Bulk),
                            volumes: vec![state.volume_id],
                        });
                    }
                    _ => {}
                }
            }
        }

        if actions.is_empty() {
            actions.push(EscalationAction::NoOp);
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        deadband::{StorageDeadband, VolumeDeadbandConfig},
        tiering::StorageTier,
    };
    use crate::internal::sd_core::{Volume, VolumeFingerprint, VolumeType};
    use std::path::PathBuf;

    fn full_volume(id: Uuid) -> std::sync::Arc<Volume> {
        let mut vol = Volume::new(id, VolumeFingerprint(id.to_string()), "Full Vol".into(), PathBuf::from("/mnt/full"));
        vol.total_capacity = 1_000_000_000_000;
        vol.available_space = 30_000_000_000; // 97% full
        vol.volume_type = VolumeType::Primary;
        vol.read_speed_mbps = Some(100);
        vol.write_speed_mbps = Some(80);
        std::sync::Arc::new(vol)
    }

    #[test]
    fn escalation_engine_triggers_migration() {
        let mut engine = EscalationEngine::new(EscalationConfig {
            auto_migrate: true,
            cooldown: Duration::from_secs(0),
            ..Default::default()
        });

        let full_id = Uuid::new_v4();
        let archive_id = Uuid::new_v4();

        let mut stripe = StorageStripe::new();
        stripe.add_volume(
            full_id,
            "NVMe".into(),
            StorageTier::Hot,
            30_000_000_000,
            1_000_000_000_000,
            vec![],
        );
        stripe.add_volume(
            archive_id,
            "S3 Cloud".into(),
            StorageTier::Archive,
            500_000_000_000_000,
            1_000_000_000_000_000,
            vec![],
        );

        let vol = full_volume(full_id);
        let mut deadband = StorageDeadband::new();
        deadband.set_config(full_id, VolumeDeadbandConfig {
            util_threshold: 0.90,
            util_warn_at: 0.80,
            ..Default::default()
        });

        let deadband_states = vec![StorageDeadbandState {
            volume_id: full_id,
            volume_name: "NVMe".into(),
            volume_type: VolumeType::Primary,
            utilization_pct: 97.0,
            deadband_state: DeadbandState::Exceeded,
            message: "97% full".into(),
        }];

        let actions = engine.process_deadband(&deadband_states, &mut stripe);
        assert!(!actions.is_empty());

        let has_migrate = actions.iter().any(|a| matches!(a, EscalationAction::Migrate { .. }));
        assert!(has_migrate, "Expected at least one Migrate action");
    }
}
