//! # Spacedrive × CoCapn — File Fleet Manager
//!
//! Integration layer that makes every Spacedrive node also a CoCapn compute tier.
//! Your NAS is a Backbone. Your laptop is a Cortex. Your cloud volume is a Cloud tier.
//! Deadband triggers notice when storage fills. Stripe escalations push files up.
//! Crossfade handoffs move data between tiers *while the user keeps editing*.
//!
//! ## The Story
//!
//! > My NAS is full. CoCapn noticed before I did — the storage deadband tripped from
//! > green (Normal) to yellow (Approaching) overnight. By morning the stripe escalated:
//! > "NAS → Backblaze B2 cloud tier." CoCapn crossfaded the cold files while I was asleep.
//! > I woke up to a notification: "4.2 TB migrated. No interrupted workflows."
//!
//! ## Architecture
//!
//! ```text
//! Spacedrive Volume  ─→  CoCapn DeviceTier  ─→  Fleet storage Stripe
//! Primary (SSD)           Backbone                Hot tier (cache)
//! Network (NAS)           Backbone                Bulk tier
//! Cloud (S3/B2)           Cloud                   Archive tier
//! Laptop (fast NVMe)      Cortex                  Working-set tier
//! ```
//!
//! ## Module Map
//!
//! | Module | Maps Spacedrive → CoCapn |
//! |--------|--------------------------|
//! | [`tiering`] | Volume classification → DeviceTier |
//! | [`deadband`] | Free-space/health → Deadband triggers |
//! | [`stripe`] | Storage tiers → Ordered compute striping |
//! | [`handoff`] | File migration → Crossfade Handoff |
//! | [`escalation`] | Fill escalation → Stripe rebalance |
//! | [`fleet`] | The FleetManager — one controller per library |

/// Internal stubs for standalone compilation.
pub mod internal;

pub mod tiering;
pub mod deadband;
pub mod stripe;
pub mod handoff;
pub mod escalation;
pub mod fleet;

// Re-export the key types users interact with
pub use deadband::{StorageDeadband, StorageDeadbandState};
pub use escalation::EscalationAction;
pub use fleet::{FleetConfig, FleetEvent, FleetManager};
pub use handoff::StorageHandoff;
pub use stripe::{StorageStripe, StorageStripeEvent, TierProfile};
pub use tiering::{
    classify_node_tier, classify_volume_tier, CoCapnNodeRole, StorageTier,
};
