//! ## Storage Deadband: free-space and health triggers
//!
//! Wraps CoCapn's `Deadband` around Spacedrive volume metrics.
//!
//! ### What it monitors
//!
//! - **Free space**: % utilization vs. configurable threshold. One-sided `Below` deadband.
//! - **Read/write speed**: degraded I/O triggers `Approaching` before total failure.
//! - **Volumes**: each tracked volume gets its own deadband.

use crate::internal::cocapn_core::{Deadband, DeadbandDirection, DeadbandState};
use crate::internal::sd_core::{Volume, VolumeType};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Outcome of a deadband check on a single volume.
#[derive(Debug, Clone)]
pub struct StorageDeadbandState {
    pub volume_id: Uuid,
    pub volume_name: String,
    pub volume_type: VolumeType,
    pub utilization_pct: f64,
    pub deadband_state: DeadbandState,
    pub message: String,
}

/// Per-volume deadband configuration.
#[derive(Debug, Clone)]
pub struct VolumeDeadbandConfig {
    /// Utilization threshold as a fraction (0.0–1.0).
    /// E.g. 0.85 = escalate when volume is 85% full.
    pub util_threshold: f64,
    /// Utilization at which we emit `Approaching` (0.0–1.0).
    /// E.g. 0.75 = warn at 75% full.
    pub util_warn_at: f64,
    /// Whether to also monitor read/write speed degradation.
    pub monitor_speed: bool,
    /// Min acceptable read speed in MB/s before triggering degradation.
    pub min_read_mbps: u64,
    /// Min acceptable write speed in MB/s before triggering degradation.
    pub min_write_mbps: u64,
}

impl Default for VolumeDeadbandConfig {
    fn default() -> Self {
        Self {
            util_threshold: 0.85,
            util_warn_at: 0.75,
            monitor_speed: true,
            min_read_mbps: 10,
            min_write_mbps: 10,
        }
    }
}

/// Default deadband config per storage tier.
impl VolumeDeadbandConfig {
    /// Sensible defaults per tier.
    pub fn for_tier(tier: &super::tiering::StorageTier) -> Self {
        match tier {
            super::tiering::StorageTier::Hot => Self {
                util_threshold: 0.80,
                util_warn_at: 0.70,
                ..Default::default()
            },
            super::tiering::StorageTier::Bulk => Self {
                util_threshold: 0.90,
                util_warn_at: 0.80,
                ..Default::default()
            },
            super::tiering::StorageTier::Archive => Self {
                util_threshold: 0.95,
                util_warn_at: 0.85,
                min_read_mbps: 1,
                min_write_mbps: 1,
                ..Default::default()
            },
            super::tiering::StorageTier::Offload => Self {
                util_threshold: 0.95,
                util_warn_at: 0.85,
                monitor_speed: false,
                ..Default::default()
            },
        }
    }
}

/// A fleet-wide storage deadband that checks every volume.
#[derive(Debug)]
pub struct StorageDeadband {
    /// Per-volume deadband config keyed by volume UUID.
    configs: HashMap<Uuid, VolumeDeadbandConfig>,
    /// Whether any volume has ever exceeded its deadband since last reset.
    has_fired: RwLock<bool>,
}

impl Default for StorageDeadband {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageDeadband {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            has_fired: RwLock::new(false),
        }
    }

    /// Set config for a specific volume.
    pub fn set_config(&mut self, volume_id: Uuid, config: VolumeDeadbandConfig) {
        self.configs.insert(volume_id, config);
    }

    /// Check all volumes and return those in non-normal states.
    pub async fn check_all(&self, volumes: &[Arc<Volume>]) -> Vec<StorageDeadbandState> {
        let mut results = Vec::new();

        for vol in volumes.iter() {
            let config = self
                .configs
                .get(&vol.id)
                .cloned()
                .unwrap_or_else(|| {
                    VolumeDeadbandConfig::for_tier(&super::tiering::classify_volume_tier(
                        vol.volume_type,
                    ))
                });

            let utilization = vol.utilization_percentage() / 100.0; // normalize to 0..1

            // Deadband: one-sided Below — we only care about space *running out*
            let db = Deadband::new(0.0, config.util_threshold, DeadbandDirection::Below);

            // We map utilization as "distance from full":
            //   utilization = 0.85 means 15% free → check(0.15, center=0.0, tolerance=0.85)
            //   If free space drops below threshold, trigger.
            let free_space = 1.0 - utilization;
            let state = db.check(free_space);

            // Speed check
            let speed_ok = if config.monitor_speed {
                let read_ok = vol
                    .read_speed_mbps
                    .map(|s| s >= config.min_read_mbps)
                    .unwrap_or(true);
                let write_ok = vol
                    .write_speed_mbps
                    .map(|s| s >= config.min_write_mbps)
                    .unwrap_or(true);
                read_ok && write_ok
            } else {
                true
            };

            let combined_state = if !speed_ok && state == DeadbandState::Normal {
                DeadbandState::Approaching
            } else if !speed_ok && state == DeadbandState::Approaching {
                DeadbandState::Exceeded
            } else {
                state
            };

            if combined_state != DeadbandState::Normal {
                let message = match combined_state {
                    DeadbandState::Approaching => {
                        format!(
                            "Volume '{}' is {:.1}% full — approaching threshold ({}%). Speed ok: {}",
                            vol.display_name(),
                            utilization * 100.0,
                            config.util_threshold * 100.0,
                            speed_ok,
                        )
                    }
                    DeadbandState::Exceeded => {
                        format!(
                            "Volume '{}' is {:.1}% full — EXCEEDED threshold ({}%). Speed ok: {}",
                            vol.display_name(),
                            utilization * 100.0,
                            config.util_threshold * 100.0,
                            speed_ok,
                        )
                    }
                    DeadbandState::Normal => unreachable!(),
                };

                let mut fired = self.has_fired.write().await;
                *fired = true;

                results.push(StorageDeadbandState {
                    volume_id: vol.id,
                    volume_name: vol.display_name().to_string(),
                    volume_type: vol.volume_type,
                    utilization_pct: utilization * 100.0,
                    deadband_state: combined_state,
                    message,
                });
            }
        }

        results
    }

    /// Reset the fired flag (after handling escalation).
    pub async fn reset_fired(&self) {
        let mut fired = self.has_fired.write().await;
        *fired = false;
    }

    /// Check if any volume has exceeded its deadband since last reset.
    pub async fn has_fired(&self) -> bool {
        *self.has_fired.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::sd_core::{Volume, VolumeFingerprint};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn near_full_volume() -> Arc<Volume> {
        let mut vol = Volume::new(
            Uuid::new_v4(),
            VolumeFingerprint("test".into()),
            "NearFull".into(),
            PathBuf::from("/mnt/nearfull"),
        );
        vol.total_capacity = 1_000_000_000_000; // 1 TB
        vol.available_space = 50_000_000_000; // 50 GB free = 95% full
        vol.volume_type = VolumeType::Secondary;
        vol.read_speed_mbps = Some(100);
        vol.write_speed_mbps = Some(80);
        Arc::new(vol)
    }

    fn healthy_volume() -> Arc<Volume> {
        let mut vol = Volume::new(
            Uuid::new_v4(),
            VolumeFingerprint("healthy".into()),
            "Healthy".into(),
            PathBuf::from("/mnt/healthy"),
        );
        vol.total_capacity = 1_000_000_000_000;
        vol.available_space = 600_000_000_000; // 40% full
        vol.volume_type = VolumeType::Primary;
        vol.read_speed_mbps = Some(300);
        vol.write_speed_mbps = Some(250);
        Arc::new(vol)
    }

    #[tokio::test]
    async fn near_full_triggers_exceeded() {
        let db = StorageDeadband::new();
        let vols = vec![near_full_volume()];
        let results = db.check_all(&vols).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].deadband_state, DeadbandState::Exceeded);
    }

    #[tokio::test]
    async fn healthy_volume_stays_normal() {
        let db = StorageDeadband::new();
        let vols = vec![healthy_volume()];
        let results = db.check_all(&vols).await;
        assert_eq!(results.len(), 0); // nothing returned = all normal
    }
}
