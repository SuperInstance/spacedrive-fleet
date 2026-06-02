/// Basic tier management example.
///
/// Shows how to configure storage tiers, register volumes with different
/// classes (Hot SSD, Bulk NAS, Archive Cloud), and check health with
/// deadband monitoring.  Run with: `cargo run --example basic_tier_management`

use cocapn_fleet::{
    deadband::VolumeDeadbandConfig,
    fleet::{FleetConfig, FleetEvent, FleetManager},
    tiering::StorageTier,
};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn make_volume(
    name: &str,
    total: u64,
    free: u64,
    volume_type: cocapn_fleet::internal::sd_core::VolumeType,
) -> Arc<cocapn_fleet::internal::sd_core::Volume> {
    let mut vol = cocapn_fleet::internal::sd_core::Volume::new(
        Uuid::new_v4(),
        cocapn_fleet::internal::sd_core::VolumeFingerprint(name.into()),
        name.into(),
        std::path::PathBuf::from("/mnt"),
    );
    vol.total_capacity = total;
    vol.available_space = free;
    vol.volume_type = volume_type;
    vol.read_speed_mbps = Some(100);
    vol.write_speed_mbps = Some(80);
    Arc::new(vol)
}

#[tokio::main]
async fn main() {
    println!("=== Basic Tier Management ===\n");

    // 1. Configure fleet with deadband thresholds
    let mut config = FleetConfig::default();
    config.check_interval = Duration::from_secs(60);

    // Customise deadband for each tier
    // Bulk storage (NAS) — warn at 80%, escalate at 85%
    let bulk_cfg = VolumeDeadbandConfig {
        util_warn_at: 0.80,
        util_threshold: 0.85,
        ..VolumeDeadbandConfig::for_tier(&StorageTier::Bulk)
    };
    config.volume_deadband_configs.insert(
        /* key is filled at registration time */ Uuid::nil(),
        bulk_cfg,
    );

    // 2. Create the fleet manager
    let (mut fleet, mut events) = FleetManager::new(config);

    // 3. Register volumes across tiers
    let ssd  = make_volume("NVMe SSD",   1_000_000_000_000,   800_000_000_000, cocapn_fleet::internal::sd_core::VolumeType::Primary);
    let nas  = make_volume("Synology",  10_000_000_000_000, 1_500_000_000_000, cocapn_fleet::internal::sd_core::VolumeType::Network);
    let b2   = make_volume("Backblaze",100_000_000_000_000,90_000_000_000_000, cocapn_fleet::internal::sd_core::VolumeType::Cloud);

    fleet.register_volume(ssd);
    fleet.register_volume(nas);
    fleet.register_volume(b2);

    println!("Registered 3 volumes: SSD (Hot), Synology (Bulk), Backblaze (Archive)\n");

    // 4. Check health
    let results = fleet.check_health().await;
    println!("Health check returned {} events:\n", results.len());
    for event in &results {
        match event {
            FleetEvent::HealthCheck { total_volumes, healthy, warning, critical } => {
                println!("  Health: {}/{} healthy, {} warning, {} critical",
                    healthy, total_volumes, warning, critical);
            }
            FleetEvent::DeadbandWarn { volume_name, utilization_pct } => {
                println!("  ⚠ Deadband WARNING on {} ({:.1}% full)", volume_name, utilization_pct);
            }
            FleetEvent::DeadbandExceeded { volume_name, utilization_pct } => {
                println!("  🚨 Deadband EXCEEDED on {} ({:.1}% full) — escalation needed", volume_name, utilization_pct);
            }
            FleetEvent::HandoffStarted { from_name, to_name, .. } => {
                println!("  🔁 Handoff started: {} → {}", from_name, to_name);
            }
            _ => {}
        }
    }

    // 5. Drain remaining channel events
    while let Ok(event) = events.try_recv() {
        match event {
            FleetEvent::HandoffProgress { from_volume: _, progress_pct, .. } => {
                println!("  Handoff progress ({:.1}%)", progress_pct);
            }
            FleetEvent::HandoffComplete { .. } => {
                println!("  ✅ Handoff complete");
            }
            _ => {}
        }
    }
}
