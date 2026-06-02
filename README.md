# 🛰️ Spacedrive Fleet — Intelligent Storage Tier Management

[![CI](https://github.com/SuperInstance/spacedrive-fleet/actions/workflows/ci.yml/badge.svg)](https://github.com/SuperInstance/spacedrive-fleet/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cocapn-fleet)](https://crates.io/crates/cocapn-fleet)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Your NAS is full. You find out when the backup fails at 3 AM. This crate prevents that.**

`cocapn-fleet` is a Rust library that transforms [Spacedrive](https://spacedrive.com)'s distributed filesystem into an intelligent, self-managing storage fleet. It monitors volume utilization across tiers, warns before things fill up, and automatically migrates cold data to cheaper storage — without waking anyone up.

Built on the device-tiering model from [CoCapn Core](https://crates.io/crates/cocapn-core), this crate gives Spacedrive operator-level storage management with zero configuration required.

---

## The Problem

Managing storage across multiple devices and clouds is painful:

| Situation | What happens |
|-----------|-------------|
| **Home NAS fills up** | Backup fails at 3 AM. You SSH in to find logs, panic, delete things blindly. |
| **SSD gets full** | Docker containers crash. The NVRAM cache stops. Your Plex metadata corrupts. |
| **Cloud bucket reaches cap** | Uploads silently fail. You don't notice until a user complains. |
| **Laptop runs out of space** | Git operations fail mid-commit. Your `.vimrc` is safe, but your Docker images aren't. |

Each case requires manual intervention. `cocapn-fleet` eliminates the manual part.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     StorageNode                              │
│   (volume metadata, type, speed, capacity, free space)      │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                    TierClassifier                             │
│   Primary → Hot  │  Network → Bulk  │  Cloud → Archive      │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                   DeadbandMonitor                            │
│   Warn at 80% │  Escalate at 85% │  Monitor speed too       │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                   EscalationChain                            │
│   Hot → Bulk → Archive → Offload (automatic routing)       │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                       Handoff                                │
│   Draining → Migrating → Settling → Complete                │
└─────────────────────────────────────────────────────────────┘
```

**Flow:** Every volume is classified by tier. A deadband monitor checks utilization and speed. When a threshold is crossed, the escalation chain finds the next healthy tier. A handoff begins — data moves without interruption. When it's done, the source volume is back under threshold.

---

## Quick Start

```rust
use cocapn_fleet::{
    deadband::{StorageDeadband, VolumeDeadbandConfig},
    fleet::{FleetConfig, FleetManager},
    stripe::StorageStripe,
    tiering::StorageTier,
};
use uuid::Uuid;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    // Configure the fleet
    let config = FleetConfig {
        check_interval: Duration::from_secs(300),
        ..Default::default()
    };
    let (mut fleet, _events) = FleetManager::new(config);

    // Register volumes (real Spacedrive Volume objects)
    let volume = make_volume("NAS01", 10_000_000_000_000, 2_000_000_000_000, VolumeType::Network);
    fleet.register_volume(volume);

    // Check health — triggers deadband, escalation, and handoff
    fleet.check_health().await;
}
```

> See [`examples/basic_tier_management.rs`](examples/basic_tier_management.rs) for a complete runnable example.

---

## User Guide

### Configuring Storage Tiers

Tiers are set automatically based on Spacedrive's volume type:

| Volume Type | Storage Tier | Typical Hardware | Deadband Threshold |
|-------------|-------------|-----------------|-------------------|
| `Primary`   | **Hot**     | NVMe SSD, NVRAM | 80% (warn), 90% (critical) |
| `Secondary` | **Hot**     | SATA SSD        | 80% (warn), 90% (critical) |
| `Network`   | **Bulk**    | NAS, SAN, DAS   | 80% (warn), 90% (critical) |
| `Cloud`     | **Archive** | S3, B2, GCS     | 85% (warn), 95% (critical) |
| `External`  | **Offload** | USB HDD, tape   | 85% (warn), 95% (critical) |

Deadband thresholds are configurable per volume — override the defaults:

```rust
let mut config = FleetConfig::default();
config.volume_deadband_configs.insert(volume_id, VolumeDeadbandConfig {
    util_threshold: 0.90,   // Escalate when 90% full
    util_warn_at: 0.80,     // Warn at 80%
    monitor_speed: true,    // Also check for slow I/O
    min_speed_mbps: Some(10), // Warn if reads below 10 MB/s
});
```

### Escalation Policies

When a volume exceeds its deadband, the escalation engine finds the best destination:

1. **Explicit escalation targets** — if the volume config maps volume A → volume B, data goes there
2. **Automatic next-tier routing** — if no explicit target, data flows to the next tier up:
   - `Hot → Bulk → Archive → Offload`
3. **Cooldown** — prevents repeated escalations. Default: 5 minutes, configurable
4. **Dry-run mode** — logs what *would* happen without taking action

```rust
config.dry_run = true;
config.escalation.cooldown = Duration::from_secs(3600);  // 1 hour
```

### Handoff Lifecycle

A storage handoff goes through these states:

| State | Meaning |
|-------|---------|
| `Stable` | No migration active |
| `Draining` | Source is locking files, preparing transfer |
| `Migrating` | Data is copying to destination |
| `Settling` | File pointers are updated, source drains |
| `Complete` | Migration done, source can be reused |
| `Cancelled` | Migration aborted (e.g., destination filled) |

---

## Templates

### Home NAS Setup

```rust
// A Synology DS920+ watching a 4-drive RAID, with Backblaze B2 as overflow
// Warn at 80%, escalate at 85%
// Cold files older than 30 days → B2
// All automatic, no cron jobs
```

See [`examples/nas_fill_scenario.rs`](examples/nas_fill_scenario.rs).

### Small Office

```rust
// 3 Mac Minis (Cortex nodes) + Synology RS1221+ (Backbone) + Wasabi S3 (Archive)
// • Mac Minis are Hot — local SSDs for active files
// • Synology is Bulk — shared storage for the team
// • Wasabi is Archive — long-term retention, compliance backups
// Deadband: warn at 70%, escalate at 80% on Hot; warn 85%, escalate 90% on Bulk
```

### Distributed Team

```rust
// 12 workstations (Cortex) + 3 NAS (Backbone) + GCS Nearline (Archive) + GCS Coldline (Offload)
// • Workstations are ephemeral — checked for overflow every 10 minutes
// • NAS is the working set — hot files stay local
// • Nearline is for stale data (>90 days)
// • Coldline is for compliance (>1 year)
```

---

## Real-World Scenario Walkthrough

**Scene:** It's a quiet Thursday evening. Your Synology DS920+ has been humming along for 8 months. You get a notification — a massive project archive just landed.

```
Time    Event
───     ─────
18:00   Project "Aurora" archive lands on Synology: 1.2 TB of 4K video rushes.
18:01   Synology utilization jumps from 74% → 86%.
18:02   Spacedrive-fleet runs its periodic health check (every 5 min default).
18:02   DeadbandMonitor: Synology at 86% — exceeds 85% threshold.
        📊 85% ⚠ Escalate: Synology → Backblaze B2
18:03   EscalationChain picks B2 as the healthiest Archive target.
18:04   Handoff begins: Draining → Migrating (old project files, 640 GB).
18:14   Handoff reaches 100%. Settling.
18:15   Synology back to 80% utilization. Handoff Complete.
        ✅ No human touch. No alert fatigue. No 3am SSH sessions.
```

The next morning, you check your dashboard and see the event log:
- `DeadbandExceeded { Synology, 86% }` → `Escalated { Bulk→Archive }` → `HandoffComplete { 640 GB }`

Everything just worked.

---

## Comparison

| Approach | Detection | Action | Downtime | Setup |
|----------|-----------|--------|----------|-------|
| **Manual monitoring** | You check `df -h` when things feel slow | You manually `rsync` to another drive | Service disrupting | None |
| **Basic alerts** | Nagios / Grafana notification at 3 AM | You SSH in, find space, fumble | 15-60 minutes of panic | Medium |
| **Cron script** | `df -h` in a cron job | Runs a predefined `rsync` command | Brief interruption during copy | High maintenance |
| **Spacedrive-fleet** | Real-time deadband at any threshold | Automatic tier-aware migration | Zero (live handoff) | 3 lines of config |

---

## Test Suite

55 tests covering every path:

```
18 unit tests    (core logic for each module)
37 integration tests   (end-to-end scenarios)
```

Coverage includes:
- Fleet: register/unregister, health checks, event broadcasting
- Tiering: device form factor classification, volume type → tier mapping
- Deadband: thresholds, speed degradation, compound warnings
- Escalation: cooldowns, auto-migrate toggle, degraded/no-target edge cases
- Stripe: multi-volume per tier, fallback paths, tier queries
- Handoff: begin/cancel lifecycle, byte progress, state transitions,
  edge cases (zero bytes, cancel-after-complete, begin-twice)
- Full integration: NAS-fill scenario routing Hot→Bulk→Archive

---

## Cargo Features

| Feature | Description | Default |
|---------|-------------|---------|
| `default` | Standard configuration | ✓ |
| — | (no optional features yet) | |

---

## License

MIT — see [LICENSE](LICENSE).

`cocapn-fleet` is part of the [Spacedrive](https://spacedrive.com) ecosystem. Built with ❤️ on top of [CoCapn Core](https://github.com/SuperInstance/cocapn-core).

---

## Related Crates

- [`cocapn-core`](https://crates.io/crates/cocapn-core) — Device tiering, deadband triggers, compute striping
- [`spacedrive`](https://github.com/spacedriveapp/spacedrive) — The universal file explorer
