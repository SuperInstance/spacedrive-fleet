//! Stub types for CoCapn core concepts.
//!
//! CoCapn is a cross-device compute orchestration framework.
//! These types model the storage/compute domain concepts used by cocapn-fleet.
//!
//! ## Deadband
//!
//! A deadband monitors a signal and fires when it leaves a normal range.
//! For storage: we monitor free space. When it drops below a threshold,
//! the deadband fires: Normal → Approaching → Exceeded.
//!
//! ## DeviceTier
//!
//! Every device in the fleet gets a tier based on its hardware role:
//! | Tier | Role |
//! |------|------|
//! | **Backbone** | Always-on, bulk storage |
//! | **Cortex** | Fast compute, GPU-capable |
//! | **Cloud** | API-driven, infinite capacity |
//! | **Reflex** | Intermittent, limited edge device |

use std::fmt;

/// Direction of the deadband.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadbandDirection {
    /// Only trip when the signal is *below* center - tolerance (one-sided).
    Below,
    /// Only trip when the signal is *above* center + tolerance (one-sided).
    Above,
    /// Trip when the signal is outside center ± tolerance (two-sided).
    Either,
}

/// State of a deadband check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadbandState {
    /// Signal is within normal range.
    Normal,
    /// Signal is approaching the threshold (within warning zone).
    Approaching,
    /// Signal has exceeded the threshold.
    Exceeded,
}

impl fmt::Display for DeadbandState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeadbandState::Normal => write!(f, "Normal"),
            DeadbandState::Approaching => write!(f, "Approaching"),
            DeadbandState::Exceeded => write!(f, "Exceeded"),
        }
    }
}

/// A deadband monitors a floating-point signal.
///
/// - `center`: the ideal/nominal value.
/// - `tolerance`: acceptable deviation from center.
/// - `direction`: which side(s) matter.
///
/// For the **Below** direction, the deadband convention used in cocapn-fleet
/// is: the signal represents *free space* (0.0 = full, 1.0 = empty) and
/// `tolerance` represents the *utilization threshold* (e.g., 0.85 = 85%).
/// The effective trigger point is `1.0 - tolerance` — free space dropping
/// below that value triggers an Exceeded state.
#[derive(Debug, Clone, Copy)]
pub struct Deadband {
    center: f64,
    tolerance: f64,
    direction: DeadbandDirection,
}

impl Deadband {
    pub fn new(center: f64, tolerance: f64, direction: DeadbandDirection) -> Self {
        Self {
            center,
            tolerance,
            direction,
        }
    }

    /// Check a signal value against the deadband.
    pub fn check(&self, signal: f64) -> DeadbandState {
        let half_tol = self.tolerance * 0.5;
        let upper = self.center + self.tolerance;
        let lower = self.center - self.tolerance;
        let upper_warn = self.center + half_tol;
        let lower_warn = self.center - half_tol;

        match self.direction {
            DeadbandDirection::Below => {
                // Below direction with center=0 and tolerance > 0.5 uses the
                // storage convention: tolerance = utilization threshold.
                // Trigger when free_space < 1.0 - tolerance.
                //
                // Example: center=0, tolerance=0.85, check(free_space=0.05).
                //   threshold = 1.0 - 0.85 = 0.15
                //   signal=0.05 < 0.15 → Exceeded (95% full, past 85% threshold)
                //
                // Example: center=0, tolerance=0.85, check(free_space=0.60).
                //   threshold = 0.15
                //   signal=0.60 >= 0.15 → Normal (40% full, well below threshold)
                if self.center.abs() < f64::EPSILON && self.tolerance > 0.5 {
                    let threshold = 1.0 - self.tolerance;
                    let warn_at = threshold * 1.5;
                    if signal <= threshold {
                        DeadbandState::Exceeded
                    } else if signal <= warn_at {
                        DeadbandState::Approaching
                    } else {
                        DeadbandState::Normal
                    }
                } else {
                    // Standard below-direction: signal below center - tolerance.
                    if signal <= lower {
                        DeadbandState::Exceeded
                    } else if signal <= lower_warn {
                        DeadbandState::Approaching
                    } else {
                        DeadbandState::Normal
                    }
                }
            }
            DeadbandDirection::Above => {
                if signal >= upper {
                    DeadbandState::Exceeded
                } else if signal >= upper_warn {
                    DeadbandState::Approaching
                } else {
                    DeadbandState::Normal
                }
            }
            DeadbandDirection::Either => {
                if signal <= lower || signal >= upper {
                    DeadbandState::Exceeded
                } else if signal <= lower_warn || signal >= upper_warn {
                    DeadbandState::Approaching
                } else {
                    DeadbandState::Normal
                }
            }
        }
    }
}

/// Device tier — what role does this node play in the fleet?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceTier {
    /// Always-on node, high storage capacity (NAS, home server).
    Backbone,
    /// Fast compute node, GPU-capable (desktop, workstation).
    Cortex,
    /// Cloud / API-driven node (S3, B2, GCS).
    Cloud,
    /// Intermittent, limited edge device (phone, tablet).
    Reflex,
}

impl fmt::Display for DeviceTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceTier::Backbone => write!(f, "Backbone"),
            DeviceTier::Cortex => write!(f, "Cortex"),
            DeviceTier::Cloud => write!(f, "Cloud"),
            DeviceTier::Reflex => write!(f, "Reflex"),
        }
    }
}

impl DeviceTier {
    pub fn display_name(&self) -> &'static str {
        match self {
            DeviceTier::Backbone => "Backbone (always-on / NAS)",
            DeviceTier::Cortex => "Cortex (fast compute / desktop)",
            DeviceTier::Cloud => "Cloud (API-driven storage)",
            DeviceTier::Reflex => "Reflex (intermittent / edge)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_exceeded() {
        // Storage: free_space = 0.05 (95% full). Threshold = 85% util → free < 0.15.
        let db = Deadband::new(0.0, 0.85, DeadbandDirection::Below);
        assert_eq!(db.check(0.05), DeadbandState::Exceeded);
    }

    #[test]
    fn below_normal() {
        // Storage: free_space = 0.60 (40% full). Well above threshold.
        let db = Deadband::new(0.0, 0.85, DeadbandDirection::Below);
        assert_eq!(db.check(0.60), DeadbandState::Normal);
    }

    #[test]
    fn below_approaching() {
        // Storage: free_space = 0.18 (82% full). Slightly above threshold (0.15).
        // This is in the warning zone between threshold and threshold*1.5.
        let db = Deadband::new(0.0, 0.85, DeadbandDirection::Below);
        assert_eq!(db.check(0.18), DeadbandState::Approaching);
    }

    #[test]
    fn above_exceeded() {
        let db = Deadband::new(50.0, 10.0, DeadbandDirection::Above);
        assert_eq!(db.check(65.0), DeadbandState::Exceeded);
    }

    #[test]
    fn above_normal() {
        let db = Deadband::new(50.0, 10.0, DeadbandDirection::Above);
        assert_eq!(db.check(45.0), DeadbandState::Normal);
    }
}
