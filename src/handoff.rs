//! ## Storage Handoff: crossfade migration between volumes
//!
//! Maps CoCapn's `Handoff` to file migration. When storage fills, migration
//! doesn't switch instantly — it crossfades, like a DJ transitioning tracks.
//!
//! ### States
//!
//! ```text
//! Stable → Draining (source draining) → Migrating (both active)
//!   → Settling (destination absorbing) → Complete
//! ```
//!
//! During any crossfade, the user can still:
//! - Read/write files on the original volume (CoCapn pushes read-requests down)
//! - Cancel the handoff (reverses gracefully)
//! - Track progress via `progress()`

use std::time::Duration;
use uuid::Uuid;

/// State of a storage migration handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageHandoffState {
    /// No migration in progress.
    Stable,
    /// Source volume is being drained (files marked for migration).
    Draining,
    /// Both volumes active; migration is mid-way.
    Migrating,
    /// Destination is absorbing final files.
    Settling,
    /// Migration complete.
    Complete,
    /// Migration was cancelled — rolled back to source.
    Cancelled,
}

/// A crossfade handoff between two storage volumes.
#[derive(Debug, Clone)]
pub struct StorageHandoff {
    pub from_volume: Uuid,
    pub to_volume: Uuid,
    pub state: StorageHandoffState,
    pub total_bytes: u64,
    pub migrated_bytes: u64,
    pub transition_duration: Duration,
    /// Which files/folders were involved (by Spacedrive entry UUID).
    pub entries: Vec<Uuid>,
    /// CoCapn-style phase tracking (0.0–1.0)
    progress_weight: f64,
}

impl StorageHandoff {
    pub fn new(
        from_volume: Uuid,
        to_volume: Uuid,
        total_bytes: u64,
        transition_duration: Duration,
    ) -> Self {
        Self {
            from_volume,
            to_volume,
            state: StorageHandoffState::Stable,
            total_bytes,
            migrated_bytes: 0,
            transition_duration,
            entries: Vec::new(),
            progress_weight: 0.0,
        }
    }

    /// Begin the handoff.
    pub fn begin(&mut self) -> Result<(), String> {
        if self.state != StorageHandoffState::Stable {
            return Err(format!(
                "Cannot begin handoff from state {:?}",
                self.state
            ));
        }
        self.state = StorageHandoffState::Draining;
        self.progress_weight = 0.0;
        Ok(())
    }

    /// Advance the migration by `delta` time and `additional_migrated` bytes.
    /// Returns progress 0.0–1.0.
    pub fn progress(&mut self, delta: Duration, additional_migrated: u64) -> f64 {
        self.migrated_bytes = self.migrated_bytes.saturating_add(additional_migrated);

        // Time-based progression (CoCapn style)
        let total_secs = self.transition_duration.as_secs_f64().max(0.001);
        let time_progress =
            (delta.as_secs_f64() / total_secs).clamp(0.0, 1.0);
        self.progress_weight = (self.progress_weight + time_progress).min(1.0);

        // State transitions based on CoCapn's three-phase pattern:
        // FadingOut (0–33%) → Crossfading (33–66%) → FadingIn (66–100%)
        self.state = if self.progress_weight < 0.33 {
            StorageHandoffState::Draining
        } else if self.progress_weight < 0.66 {
            StorageHandoffState::Migrating
        } else if self.progress_weight < 1.0 {
            StorageHandoffState::Settling
        } else {
            StorageHandoffState::Complete
        };

        self.progress_weight
    }

    /// Cancel the handoff. Returns to Stable.
    pub fn cancel(&mut self) -> Result<(), String> {
        if self.state == StorageHandoffState::Complete {
            return Err("Cannot cancel a completed handoff".into());
        }
        self.state = StorageHandoffState::Cancelled;
        self.migrated_bytes = 0;
        self.progress_weight = 0.0;
        Ok(())
    }

    /// Check if the handoff is complete.
    pub fn is_complete(&self) -> bool {
        self.state == StorageHandoffState::Complete
    }

    /// Files that should still resolve to the source volume (push-down for active reads).
    pub fn active_on_source(&self) -> bool {
        matches!(
            self.state,
            StorageHandoffState::Stable
                | StorageHandoffState::Draining
                | StorageHandoffState::Migrating
        )
    }

    /// Files that also resolve on the destination volume.
    pub fn active_on_destination(&self) -> bool {
        matches!(
            self.state,
            StorageHandoffState::Migrating | StorageHandoffState::Settling
        )
    }

    /// Get byte-based progress as a float 0.0–1.0.
    pub fn byte_progress(&self) -> f64 {
        if self.total_bytes == 0 {
            return 1.0;
        }
        (self.migrated_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_lifecycle() {
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        let mut h = StorageHandoff::new(from, to, 1024 * 1024 * 100, Duration::from_secs(10));

        assert_eq!(h.state, StorageHandoffState::Stable);
        h.begin().unwrap();
        assert_eq!(h.state, StorageHandoffState::Draining);

        // 3s of 10s total → weight = 0.30 → Draining (< 0.33)
        let p = h.progress(Duration::from_secs(3), 10_000_000);
        assert!(p < 0.33, "progress should be {p} < 0.33 for Draining");
        assert_eq!(h.state, StorageHandoffState::Draining);

        // +2s → total 5s → weight = 0.50 → Migrating (0.33–0.66)
        let p = h.progress(Duration::from_secs(2), 30_000_000);
        assert!(p >= 0.33 && p < 0.66, "progress should be {p} in [0.33, 0.66) for Migrating");
        assert_eq!(h.state, StorageHandoffState::Migrating);

        // +6s → total 11s → clamped to 1.0 → Complete
        let p = h.progress(Duration::from_secs(6), 60_000_000);
        assert_eq!(p, 1.0, "progress should be 100%");
        assert_eq!(h.state, StorageHandoffState::Complete);
        assert!(h.is_complete());
    }

    #[test]
    fn cancel_during_handoff() {
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        let mut h = StorageHandoff::new(from, to, 1000, Duration::from_secs(10));

        h.begin().unwrap();
        h.progress(Duration::from_secs(3), 200);
        h.cancel().unwrap();

        assert_eq!(h.state, StorageHandoffState::Cancelled);
        assert_eq!(h.migrated_bytes, 0);
    }

    #[test]
    fn byte_progress_tracking() {
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        let h = StorageHandoff::new(from, to, 1000, Duration::from_secs(10));

        // With 0 migrated bytes and 1000 total, byte_progress should be 0.0
        assert_eq!(h.byte_progress(), 0.0, "no bytes migrated yet");
    }

    #[test]
    fn byte_progress_actual() {
        let from = Uuid::new_v4();
        let to = Uuid::new_v4();
        let mut h = StorageHandoff::new(from, to, 1000, Duration::from_secs(10));

        h.begin().unwrap();
        h.progress(Duration::ZERO, 500);
        assert!((h.byte_progress() - 0.5).abs() < 0.01);
        h.progress(Duration::ZERO, 500);
        assert!((h.byte_progress() - 1.0).abs() < 0.01);
    }
}
