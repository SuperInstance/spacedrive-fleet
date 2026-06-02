//! ## FleetManager — the single controller that runs the show
//!
//! The `FleetManager` sits in a Spacedrive node's service layer. It:
//!
//! 1. Collects volume snapshots from the `VolumeManager`
//! 2. Feeds them through the `StorageDeadband`
//! 3. If the deadband fires, consults the `StorageStripe`
//! 4. If escalation is needed, spawns a `StorageHandoff` migration
//! 5. Emits `FleetEvent`s for the Spacedrive event bus / UI
//!
//! In production, this would run as a tokio task alongside the Spacedrive daemon.

use crate::{
    deadband::{StorageDeadband, VolumeDeadbandConfig},
    escalation::{EscalationAction, EscalationConfig, EscalationEngine},
    handoff::StorageHandoff,
    stripe::StorageStripe,
    tiering::{classify_volume_tier, StorageTier},
};
use crate::internal::cocapn_core::DeadbandState;
use crate::internal::sd_core::Volume;
#[allow(unused_imports)]
use crate::internal::sd_core::VolumeType;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};
use uuid::Uuid;

/// Events the FleetManager emits to the UI / Spacedrive event bus.
#[derive(Debug, Clone)]
pub enum FleetEvent {
    /// A volume's storage deadband entered a warning state.
    DeadbandWarn {
        volume_name: String,
        utilization_pct: f64,
    },
    /// A volume is critically full — escalation triggered.
    DeadbandExceeded {
        volume_name: String,
        utilization_pct: f64,
    },
    /// A migration handoff has started.
    HandoffStarted {
        from_volume: Uuid,
        to_volume: Uuid,
        from_name: String,
        to_name: String,
    },
    /// Migration progress update.
    HandoffProgress {
        from_volume: Uuid,
        to_volume: Uuid,
        progress_pct: f64,
    },
    /// Migration completed successfully.
    HandoffComplete {
        from_volume: Uuid,
        to_volume: Uuid,
    },
    /// Migration was cancelled / rolled back.
    HandoffCancelled {
        from_volume: Uuid,
        to_volume: Uuid,
    },
    /// A tier is fully degraded.
    TierDegraded {
        tier: StorageTier,
        volumes: Vec<Uuid>,
    },
    /// Fleet health check summary.
    HealthCheck {
        total_volumes: usize,
        healthy: usize,
        warning: usize,
        critical: usize,
    },
}

/// Configuration for the FleetManager.
#[derive(Debug, Clone)]
pub struct FleetConfig {
    /// How often to check volume health (seconds).
    pub check_interval: Duration,
    /// Per-volume deadband config overrides (by volume UUID).
    pub volume_deadband_configs: HashMap<Uuid, VolumeDeadbandConfig>,
    /// Escalation engine config.
    pub escalation: EscalationConfig,
    /// Whether to run dry (no actual file migrations).
    pub dry_run: bool,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60),
            volume_deadband_configs: HashMap::new(),
            escalation: EscalationConfig::default(),
            dry_run: false,
        }
    }
}

/// The FleetManager — one controller per Spacedrive library.
#[derive(Debug)]
pub struct FleetManager {
    config: FleetConfig,
    stripe: StorageStripe,
    deadband: StorageDeadband,
    escalation_engine: EscalationEngine,
    /// Active handoffs keyed by source volume UUID.
    active_handoffs: HashMap<Uuid, StorageHandoff>,
    event_tx: Sender<FleetEvent>,
    event_rx: Option<Receiver<FleetEvent>>,
    /// Snapshot of known volumes (by UUID).
    volumes: HashMap<Uuid, Arc<Volume>>,
}

impl FleetManager {
    /// Create a new FleetManager. Returns the manager and an event receiver.
    pub fn new(config: FleetConfig) -> (Self, Receiver<FleetEvent>) {
        let (tx, rx) = mpsc::channel(256);
        let mut db = StorageDeadband::new();
        for (vid, cfg) in &config.volume_deadband_configs {
            db.set_config(*vid, cfg.clone());
        }

        (
            Self {
                config,
                stripe: StorageStripe::new(),
                deadband: db,
                escalation_engine: EscalationEngine::new(Default::default()),
                active_handoffs: HashMap::new(),
                event_tx: tx,
                event_rx: None,
                volumes: HashMap::new(),
            },
            rx,
        )
    }

    /// Register a volume (called when Spacedrive detects a new volume).
    pub fn register_volume(&mut self, volume: Arc<Volume>) {
        let tier = classify_volume_tier(volume.volume_type);
        self.stripe.add_volume(
            volume.id,
            volume.display_name().to_string(),
            tier,
            volume.available_space,
            volume.total_capacity,
            vec![], // escalation targets can be set later
        );
        self.volumes.insert(volume.id, volume);
    }

    /// Remove a volume (volume unmounted, ejected, etc).
    pub fn unregister_volume(&mut self, volume_id: Uuid) {
        self.volumes.remove(&volume_id);
        self.stripe.fail_volume(volume_id, "unregistered".into());
    }

    /// Run a single fleet health check on all registered volumes.
    pub async fn check_health(&mut self) -> Vec<FleetEvent> {
        let volumes: Vec<Arc<Volume>> = self.volumes.values().cloned().collect();
        let deadband_states = self.deadband.check_all(&volumes).await;

        // Update stripe with current free space
        for vol in &volumes {
            if let Some(profile) = self
                .stripe
                .healthy_volumes_in_tier(classify_volume_tier(vol.volume_type))
                .into_iter()
                .find(|p| p.volume_id == vol.id)
            {
                // Profile exists — just update free space in a future iteration
                // (stripe profiles are immutable after creation in current design)
            }
        }

        let mut events = Vec::new();

        // Emit deadband events
        for state in &deadband_states {
            match state.deadband_state {
                DeadbandState::Approaching => {
                    events.push(FleetEvent::DeadbandWarn {
                        volume_name: state.volume_name.clone(),
                        utilization_pct: state.utilization_pct,
                    });
                }
                DeadbandState::Exceeded => {
                    events.push(FleetEvent::DeadbandExceeded {
                        volume_name: state.volume_name.clone(),
                        utilization_pct: state.utilization_pct,
                    });
                }
                _ => {}
            }
        }

        // Escalate if needed
        let actions = self
            .escalation_engine
            .process_deadband(&deadband_states, &mut self.stripe);

        for action in &actions {
            match action {
                EscalationAction::Migrate {
                    from_volume,
                    to_volume,
                    from_tier,
                    to_tier,
                    total_bytes,
                } => {
                    let from_name = self
                        .volumes
                        .get(from_volume)
                        .map(|v| v.display_name().to_string())
                        .unwrap_or_default();
                    let to_name = self
                        .volumes
                        .get(to_volume)
                        .map(|v| v.display_name().to_string())
                        .unwrap_or_default();

                    events.push(FleetEvent::HandoffStarted {
                        from_volume: *from_volume,
                        to_volume: *to_volume,
                        from_name: from_name.clone(),
                        to_name: to_name.clone(),
                    });

                    if !self.config.dry_run {
                        let mut handoff = StorageHandoff::new(
                            *from_volume,
                            *to_volume,
                            *total_bytes,
                            Duration::from_secs(300), // 5 min default migration
                        );
                        if handoff.begin().is_ok() {
                            self.active_handoffs.insert(*from_volume, handoff);
                        }
                    }
                }
                EscalationAction::Degraded { tier, volumes } => {
                    events.push(FleetEvent::TierDegraded {
                        tier: *tier,
                        volumes: volumes.clone(),
                    });
                }
                EscalationAction::NoOp => {}
            }
        }

        // Send events to channel
        for event in &events {
            let _ = self.event_tx.send(event.clone()).await;
        }

        // Count health
        let total = volumes.len();
        let warning = deadband_states
            .iter()
            .filter(|s| s.deadband_state == DeadbandState::Approaching)
            .count();
        let critical = deadband_states
            .iter()
            .filter(|s| s.deadband_state == DeadbandState::Exceeded)
            .count();
        let healthy = total.saturating_sub(warning).saturating_sub(critical);

        events.push(FleetEvent::HealthCheck {
            total_volumes: total,
            healthy,
            warning,
            critical,
        });

        events
    }

    /// Main loop — run periodically. In production, spawned as a tokio task.
    pub async fn run_loop(mut self) {
        loop {
            self.check_health().await;
            tokio::time::sleep(self.config.check_interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::sd_core::{Volume, VolumeFingerprint};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn make_vol(name: &str, total: u64, free: u64, vtype: VolumeType, id: Uuid) -> Arc<Volume> {
        let mut vol = Volume::new(id, VolumeFingerprint(id.to_string()), name.into(), PathBuf::from("/mnt"));
        vol.total_capacity = total;
        vol.available_space = free;
        vol.volume_type = vtype;
        vol.read_speed_mbps = Some(100);
        vol.write_speed_mbps = Some(80);
        Arc::new(vol)
    }

    #[tokio::test]
    async fn health_check_reports_correctly() {
        let (mut fm, _rx) = FleetManager::new(FleetConfig::default());

        let hot_id = Uuid::new_v4();
        let bulk_id = Uuid::new_v4();

        fm.register_volume(make_vol("NVMe", 1_000_000_000_000, 800_000_000_000, VolumeType::Primary, hot_id));
        fm.register_volume(make_vol("NAS", 10_000_000_000_000, 1_000_000_000_000, VolumeType::Network, bulk_id));

        let events = fm.check_health().await;

        // Should at least have the health check event
        let health = events.iter().find(|e| matches!(e, FleetEvent::HealthCheck { .. }));
        assert!(health.is_some(), "Expected a HealthCheck event");

        if let Some(FleetEvent::HealthCheck { total_volumes, healthy, .. }) = health {
            assert_eq!(*total_volumes, 2);
            // NAS is 90% full — should be at Approaching or Exceeded
            // NVMe is 20% full — healthy
            // So healthy should be 1 and warning+critical = 1
            assert_eq!(*healthy, 1);
        }

        // The NAS volume is 90% full — Bulk tier threshold is 90%, so free space is 0.10.
        // With below-direction deadband (tolerance=0.90), signal 0.10 <= 0.90 → Exceeded.
        let has_exceeded = events.iter().any(|e| matches!(e, FleetEvent::DeadbandExceeded { .. }));
        assert!(has_exceeded, "NAS at 90% should trigger DeadbandExceeded with Bulk threshold of 90%");
    }
}
