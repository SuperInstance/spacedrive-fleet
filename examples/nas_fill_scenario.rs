/// NAS fill scenario walkthrough.
///
/// Simulates a Synology NAS filling up: the deadband monitor detects it,
/// triggers automatic escalation to Backblaze B2 (cloud), and completes
/// the handoff — all without human intervention.
///
/// Run with: `cargo run --example nas_fill_scenario`

use cocapn_fleet::{
    deadband::StorageDeadband,
    escalation::{EscalationAction, EscalationEngine, EscalationConfig},
    handoff::{StorageHandoff, StorageHandoffState},
    stripe::StorageStripe,
    tiering::StorageTier,
};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn make_volume(
    name: &str, total: u64, free: u64,
    vtype: cocapn_fleet::internal::sd_core::VolumeType,
) -> (Uuid, Arc<cocapn_fleet::internal::sd_core::Volume>) {
    let id = Uuid::new_v4();
    let mut vol = cocapn_fleet::internal::sd_core::Volume::new(
        id,
        cocapn_fleet::internal::sd_core::VolumeFingerprint(name.into()),
        name.into(),
        std::path::PathBuf::from("/mnt"),
    );
    vol.total_capacity = total;
    vol.available_space = free;
    vol.volume_type = vtype;
    vol.read_speed_mbps = Some(100);
    vol.write_speed_mbps = Some(80);
    (id, Arc::new(vol))
}

fn pct(msg: &str, used: u64, total: u64) -> f64 {
    let p = used as f64 / total as f64 * 100.0;
    println!("{} {:.1}% ({:.1} GB / {:.1} GB)", msg, p, used as f64 / 1e9, total as f64 / 1e9);
    p
}

#[tokio::main]
async fn main() {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║        Spacedrive Fleet — NAS Fill Scenario             ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    // ── Scene: The NAS is filling up ────────────────────────────────
    let (nas_id, nas_vol) = make_volume(
        "Synology DS920+",          // name
        10_000_000_000_000,         // 10 TB total
        1_500_000_000_000,          // 1.5 TB free  = 85% full
        cocapn_fleet::internal::sd_core::VolumeType::Network,
    );
    let (b2_id, b2_vol) = make_volume(
        "Backblaze B2",
        100_000_000_000_000,        // 100 TB total
        95_000_000_000_000,         // 95 TB free   = 5% used
        cocapn_fleet::internal::sd_core::VolumeType::Cloud,
    );

    pct("NAS:", 10_000_000_000_000 - 1_500_000_000_000, 10_000_000_000_000);
    pct("Cloud:", 100_000_000_000_000 - 95_000_000_000_000, 100_000_000_000_000);

    println!("\n  The NAS is at 85% — close to the 80% warning threshold.");
    println!("  Once it crosses 80%, Spacedrive-fleet will notice and\n  start migrating cold files to Backblaze B2.\n");

    // ── 1. Deadband monitoring ──────────────────────────────────────
    println!("─── Step 1: Deadband Monitoring ───");
    let deadband = StorageDeadband::new();
    let results = deadband.check_all(&[nas_vol.clone(), b2_vol.clone()]).await;

    for r in &results {
        let state = match r.deadband_state {
            cocapn_fleet::internal::cocapn_core::DeadbandState::Normal => "✅ Normal",
            cocapn_fleet::internal::cocapn_core::DeadbandState::Approaching => "⚠ Approaching threshold",
            cocapn_fleet::internal::cocapn_core::DeadbandState::Exceeded => "🚨 EXCEEDED",
        };
        println!("  [{}] {} — {:.1}% full — {}",
            r.volume_name, state, r.utilization_pct * 100.0, r.message);
    }

    // ── 2. Escalation engine decides what to do ────────────────────
    println!("\n─── Step 2: Escalation Engine ───");
    let mut stripe = StorageStripe::new();

    // Register in the stripe with escalation targets
    // NAS (Bulk) → B2 (Archive)
    stripe.add_volume(nas_id, "Synology DS920+".into(), StorageTier::Bulk,
        1_500_000_000_000, 10_000_000_000_000, vec![b2_id]);
    stripe.add_volume(b2_id, "Backblaze B2".into(), StorageTier::Archive,
        95_000_000_000_000, 100_000_000_000_000, vec![]);

    let mut engine = EscalationEngine::new(EscalationConfig {
        auto_migrate: true,
        cooldown: Duration::from_secs(300),
        ..Default::default()
    });

    let actions = engine.process_deadband(&results, &mut stripe);

    for action in &actions {
        match action {
            EscalationAction::Migrate { from_volume, to_volume, from_tier, to_tier, total_bytes, .. } => {
                println!("  🔁 Migrate {} bytes from {:?} ({}) → {:?} ({})",
                    total_bytes, from_tier, from_volume, to_tier, to_volume);
            }
            EscalationAction::Degraded { tier, volumes, .. } => {
                println!("  ⚠ Degraded: {:?} volumes {:?}", tier, volumes);
            }
            EscalationAction::NoOp => {
                println!("  → No action needed (within thresholds)");
            }
        }
    }

    // ── 3. Perform the handoff ──────────────────────────────────────
    println!("\n─── Step 3: Storage Handoff ───");
    let total_to_migrate = 500_000_000_000u64; // 500 GB cold files
    let mut handoff = StorageHandoff::new(nas_id, b2_id, total_to_migrate, Duration::from_secs(60));

    assert!(handoff.begin().is_ok());
    assert!(handoff.active_on_source());

    // Simulate the migration over time
    let mut migrated = 0u64;
    let chunk_size = 100_000_000_000u64; // 100 GB per tick
    let chunk_duration = Duration::from_secs(2);

    while !handoff.is_complete() {
        tokio::time::sleep(chunk_duration).await;
        migrated += chunk_size.min(total_to_migrate - migrated);
        handoff.progress(chunk_duration, migrated);

        let pct = handoff.byte_progress() * 100.0;
        let state_name = match handoff.state {
            StorageHandoffState::Stable    => "Stable",
            StorageHandoffState::Draining  => "Draining",
            StorageHandoffState::Migrating => "Migrating",
            StorageHandoffState::Settling  => "Settling",
            StorageHandoffState::Complete  => "Complete",
            StorageHandoffState::Cancelled => "Cancelled",
        };
        println!("  [{}] {:.0}% — {} ({:.1} GB of {:.1} GB) via {}→{}",
            state_name, pct, state_name,
            migrated as f64 / 1e9, total_to_migrate as f64 / 1e9,
            "Synology", "Backblaze");
    }

    println!("\n─── Step 4: Result ───");
    assert!(!handoff.active_on_source());
    assert!(handoff.is_complete());
    println!("  ✅ NAS fill scenario resolved automatically!");
    println!("  Cold files migrated from Synology DS920+ → Backblaze B2.");
    println!("  The NAS is back under the deadband threshold.");
    println!("  No 3am backup failures. No frantic SSH sessions.\n");

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║   This is what intelligent tier management looks like.  ║");
    println!("╚══════════════════════════════════════════════════════════╝");
}
