# 🚀 cocapn-fleet

> **Your NAS is full.** CoCapn noticed before you did — and escalated to cloud storage while you slept.

[![Crates.io](https://img.shields.io/crates/v/cocapn-fleet)](https://crates.io/crates/cocapn-fleet)
[![Docs](https://docs.rs/cocapn-fleet/badge.svg)](https://docs.rs/cocapn-fleet)
[![License: MIT OR FSL-1.1-ALv2](https://img.shields.io/badge/license-MIT%20OR%20FSL--1.1--ALv2-blue)](#license)

`cocapn-fleet` is a [Spacedrive](https://spacedrive.com) integration layer that treats every volume as a CoCapn compute tier. Your NAS is a Backbone. Your laptop is a Cortex. Your cloud bucket is an Archive tier. Deadband triggers notice when storage fills. Stripe escalations push files up. Crossfade handoffs migrate while you keep working.

## The 30-Second Story

My NAS is full. CoCapn noticed before I did — the storage deadband tripped from green (Normal) to yellow (Approaching) overnight. By morning the stripe triggered an escalation: NAS → Backblaze B2 cloud tier. CoCapn crossfaded the cold files while I was asleep. I woke up to a notification: "4.2 TB migrated. No interrupted workflows."

## How It Works

### 1. Every device gets a CoCapn tier

| Your Device | CoCapn Tier | Role |
|------------|-------------|------|
| NAS / home server | **Backbone** | Always-on, bulk storage |
| Desktop workstation | **Cortex** | GPU-capable, fast NVMe |
| Laptop | **Cortex** | Mobile compute, fast local SSD |
| Cloud volume (S3, B2, GCS) | **Cloud** | Archive tier, unlimited |
| Phone / Tablet | **Reflex** | Intermittent, edge cache |

### 2. Every volume gets a storage tier

| Volume Type | Storage Tier | CoCapn Analogue |
|------------|-------------|-----------------|
| Primary (SSD) | **Hot** | Cortex — fast, low capacity |
| Network (NAS) | **Bulk** | Backbone — high capacity |
| Cloud (S3, B2) | **Archive** | Cloud — infinite, slow |
| External USB | **Offload** | Reflex — disconnectable |

### 3. Deadband watches storage thresholds

CoCapn's deadband trigger monitors utilization:

```rust
// CoCapn's deadband, applied to storage
// Wait until free space drops below 15%.
let storage_db = Deadband::new(0.0, 0.85, DeadbandDirection::Below);
let free_space = 0.95; // 95% free

match storage_db.check(free_space) {
    DeadbandState::Normal => println!("Plenty of room."),
    DeadbandState::Approaching => println!("Getting full…"),
    DeadbandState::Exceeded => println!("Time to escalate."),
}
```

| State | What It Means |
|-------|--------------|
| `Normal` | Free space well above threshold |
| `Approaching` | Volume nearing the threshold |
| `Exceeded` | Volume past threshold — time to migrate |

### 4. The stripe knows where to go next

```
Hot (NVMe @ 95%) → Bulk (NAS @ 40%) → Archive (B2, barely touched)
```

### 5. Crossfade handoff migrates while you work

No `cp -r`. This is a DJ-style crossfade:

```
Stable → Draining → Migrating → Settling → Complete
```

Cancel anytime. Reads still work on the source. Zero interruptions.

## Quick Start

```toml
[dependencies]
cocapn-fleet = "0.1"
```

```rust
use cocapn_fleet::{FleetConfig, FleetManager};

let (mut fm, mut rx) = FleetManager::new(FleetConfig::default());

// Register volumes
fm.register_volume(my_nas_volume);
fm.register_volume(my_cloud_volume);
fm.register_volume(my_local_ssd);

// Run a health check
let events = fm.check_health().await;

// React to events
while let Some(event) = rx.recv().await {
    match event {
        FleetEvent::DeadbandExceeded { volume_name, .. } => {
            println!("⚠️  {} is critically full!", volume_name);
        }
        FleetEvent::HandoffStarted { from_name, to_name, .. } => {
            println!("🔄 Migrating {} → {}", from_name, to_name);
        }
        FleetEvent::HandoffComplete { .. } => {
            println!("✅ Migration complete.");
        }
        _ => {}
    }
}
```

## Architecture

```
src/
├── lib.rs          # Re-exports, module map
├── tiering.rs      # Device/Volume → CoCapn tier classification
├── deadband.rs     # StorageDeadband: free-space & I/O health monitoring
├── stripe.rs       # StorageStripe: ordered tier fallback
├── handoff.rs      # StorageHandoff: crossfade migration state machine
├── escalation.rs   # EscalationEngine: deadband → stripe → handoff
├── fleet.rs        # FleetManager: the single controller per library
└── internal/
    ├── mod.rs
    ├── cocapn_core.rs  # Standalone CoCapn type stubs
    └── sd_core.rs      # Standalone Spacedrive domain stubs
```

> **Note:** When integrated into Spacedrive proper, `internal/cocapn_core.rs` and `internal/sd_core.rs` are replaced by the actual `cocapn-core` and `sd-core` crates. The stubs exist for standalone compilation and testing.

## Comparison

| Situation | Spacedrive Alone | Spacedrive + CoCapn |
|-----------|-----------------|---------------------|
| NAS at 95% | You see "NAS is full" | Deadband trips. Migration starts. |
| Laptop SSD full | Manual copy to NAS | Crossfade cold files to Bulk tier |
| Cloud egress costs | Manual tiering | Stripe prefers Bulk over Archive |
| Device goes offline | Marked offline | Stripe rebalances to next healthy tier |

## License

`MIT OR FSL-1.1-ALv2` — same as Spacedrive. CoCapn core concepts are MIT-licensed.

---

*Built with ❄️ by someone who keeps running out of space on their NAS.*
