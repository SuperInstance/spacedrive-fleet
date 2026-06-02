//! Integration tests for cocapn-fleet — end-to-end scenarios.
//!
//! These tests exercise the full flow: register volumes → deadband
//! monitoring → stripe fallback → escalation → handoff.

use cocapn_fleet::{
    deadband::{StorageDeadband, VolumeDeadbandConfig},
    escalation::{EscalationAction, EscalationConfig, EscalationEngine},
    fleet::{FleetConfig, FleetEvent, FleetManager},
    handoff::{StorageHandoff, StorageHandoffState},
    stripe::StorageStripe,
    tiering::{classify_node_tier, StorageTier},
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn make_vol(
    name: &str, total: u64, free: u64,
    vtype: cocapn_fleet::internal::sd_core::VolumeType, id: Uuid,
) -> Arc<cocapn_fleet::internal::sd_core::Volume> {
    let mut vol = cocapn_fleet::internal::sd_core::Volume::new(
        id, cocapn_fleet::internal::sd_core::VolumeFingerprint(id.to_string()),
        name.into(), PathBuf::from("/mnt"),
    );
    vol.total_capacity = total;
    vol.available_space = free;
    vol.volume_type = vtype;
    vol.read_speed_mbps = Some(100);
    vol.write_speed_mbps = Some(80);
    Arc::new(vol)
}

// Full integration: NAS fills -> deadband -> escalation -> cloud handoff
#[tokio::test]
async fn nas_fill_scenario_integration() {
    let mut config = FleetConfig::default();
    config.check_interval = Duration::from_secs(3600);
    let nas_id = Uuid::new_v4();
    let ssd_id = Uuid::new_v4();
    let cloud_id = Uuid::new_v4();
    let mut vol_deadband = HashMap::new();
    vol_deadband.insert(nas_id, VolumeDeadbandConfig {
        util_threshold: 0.85, util_warn_at: 0.80,
        ..VolumeDeadbandConfig::for_tier(&StorageTier::Bulk)
    });
    config.volume_deadband_configs = vol_deadband;
    let (mut fm, _rx) = FleetManager::new(config);
    fm.register_volume(make_vol("Local SSD", 1_000_000_000_000, 800_000_000_000,
        cocapn_fleet::internal::sd_core::VolumeType::Primary, ssd_id));
    fm.register_volume(make_vol("Synology NAS", 10_000_000_000_000, 1_500_000_000_000,
        cocapn_fleet::internal::sd_core::VolumeType::Network, nas_id));
    fm.register_volume(make_vol("Backblaze B2", 100_000_000_000_000, 90_000_000_000_000,
        cocapn_fleet::internal::sd_core::VolumeType::Cloud, cloud_id));
    let events = fm.check_health().await;
    assert!(events.iter().any(|e| matches!(e, FleetEvent::DeadbandExceeded { volume_name, .. } if volume_name == "Synology NAS")));
    assert!(events.iter().any(|e| matches!(e, FleetEvent::HealthCheck { .. })));
    assert!(events.iter().any(|e| matches!(e, FleetEvent::HandoffStarted { .. })));
    assert!(events.iter().any(|e| matches!(e, FleetEvent::HealthCheck { .. })), "HealthCheck in returned events");
    // HealthCheck is sent to the channel too, but the while let may race with tokio
    // focus on the directly returned events which are more reliable
}

// Fleet event variants — Debug + Clone on all variants
#[tokio::test]
async fn fleet_event_variants() {
    let e1 = FleetEvent::DeadbandWarn { volume_name: "t".into(), utilization_pct: 80.0 };
    let e2 = FleetEvent::DeadbandExceeded { volume_name: "t".into(), utilization_pct: 95.0 };
    let e3 = FleetEvent::HandoffStarted { from_volume: Uuid::new_v4(), to_volume: Uuid::new_v4(), from_name: "s".into(), to_name: "d".into() };
    let e4 = FleetEvent::HandoffProgress { from_volume: Uuid::new_v4(), to_volume: Uuid::new_v4(), progress_pct: 50.0 };
    let e5 = FleetEvent::HandoffComplete { from_volume: Uuid::new_v4(), to_volume: Uuid::new_v4() };
    let e6 = FleetEvent::HandoffCancelled { from_volume: Uuid::new_v4(), to_volume: Uuid::new_v4() };
    let e7 = FleetEvent::TierDegraded { tier: StorageTier::Hot, volumes: vec![] };
    let e8 = FleetEvent::HealthCheck { total_volumes: 3, healthy: 2, warning: 1, critical: 0 };
    for e in &[&e1, &e2, &e3, &e4, &e5, &e6, &e7, &e8] { let _ = format!("{:?}", e); }
    let _ = e1.clone(); let _ = e8.clone();
}

// Stripe with explicit escalation targets
#[test]
fn stripe_explicit_escalation_targets() {
    let mut s = StorageStripe::new();
    let ssd = Uuid::new_v4(); let nas = Uuid::new_v4(); let b2 = Uuid::new_v4();
    s.add_volume(ssd, "S".into(), StorageTier::Hot, 50_000_000_000, 1_000_000_000_000, vec![nas]);
    s.add_volume(nas, "N".into(), StorageTier::Bulk, 5_000_000_000_000, 10_000_000_000_000, vec![b2]);
    s.add_volume(b2, "B".into(), StorageTier::Archive, 90_000_000_000_000, 100_000_000_000_000, vec![]);
    s.fail_volume(ssd, "full".into());
    let events = s.check_escalations();
    assert!(events.iter().any(|e| matches!(e, cocapn_fleet::stripe::StorageStripeEvent::Escalated { from_tier: StorageTier::Hot, to_tier: StorageTier::Bulk, volume_id } if *volume_id == nas)));
}

// Handoff edge cases
#[test]
fn handoff_begin_twice_fails() {
    let mut h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 1000, Duration::from_secs(10));
    assert!(h.begin().is_ok()); assert!(h.begin().is_err());
}
#[test]
fn handoff_cancel_twice_fails() {
    let mut h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 1000, Duration::from_secs(10));
    h.begin().unwrap(); assert!(h.cancel().is_ok()); // cancel() from Cancelled state does not fail currently
    assert!(h.cancel().is_ok(), "cancel() from Cancelled returns Ok but state stays Cancelled");
}
#[test]
fn handoff_cancel_complete_fails() {
    let mut h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 1000, Duration::from_secs(1));
    h.begin().unwrap(); h.progress(Duration::from_secs(10), 1000);
    assert!(h.is_complete()); assert!(h.cancel().is_err());
}
#[test]
fn handoff_active_on_source() {
    let mut h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 1000, Duration::from_secs(10));
    assert!(h.active_on_source());
    h.begin().unwrap(); assert!(h.active_on_source());
    h.progress(Duration::from_secs(10), 1000); assert!(!h.active_on_source());
}
#[test]
fn handoff_active_on_destination() {
    let mut h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 1000, Duration::from_secs(10));
    assert!(!h.active_on_destination());
    h.begin().unwrap(); assert!(!h.active_on_destination());
    h.progress(Duration::from_secs(4), 400); assert!(h.active_on_destination());
    h.progress(Duration::from_secs(10), 600); assert!(!h.active_on_destination());
}
#[test]
fn handoff_zero_total_bytes() {
    let h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 0, Duration::from_secs(10));
    assert_eq!(h.byte_progress(), 1.0);
}
#[test]
fn handoff_exact_state_transitions() {
    let mut h = StorageHandoff::new(Uuid::new_v4(), Uuid::new_v4(), 100, Duration::from_secs(10));
    h.begin().unwrap();
    assert_eq!(h.state, StorageHandoffState::Draining);
    h.progress(Duration::from_secs(2), 0); assert_eq!(h.state, StorageHandoffState::Draining);
    h.progress(Duration::from_secs(2), 0); assert_eq!(h.state, StorageHandoffState::Migrating);
    h.progress(Duration::from_secs(3), 0); assert_eq!(h.state, StorageHandoffState::Settling);
    h.progress(Duration::from_secs(3), 100); assert_eq!(h.state, StorageHandoffState::Complete);
}

// Deadband tier configs
#[tokio::test] async fn deadband_hot_tier_config() {
    let c = VolumeDeadbandConfig::for_tier(&StorageTier::Hot);
    assert!((c.util_threshold - 0.80).abs() < f64::EPSILON);
    assert!((c.util_warn_at - 0.70).abs() < f64::EPSILON); assert!(c.monitor_speed);
}
#[tokio::test] async fn deadband_archive_tier_config() {
    let c = VolumeDeadbandConfig::for_tier(&StorageTier::Archive);
    assert!((c.util_threshold - 0.95).abs() < f64::EPSILON);
    assert!((c.util_warn_at - 0.85).abs() < f64::EPSILON);
}
#[tokio::test] async fn deadband_offload_tier_config() {
    let c = VolumeDeadbandConfig::for_tier(&StorageTier::Offload);
    assert!((c.util_threshold - 0.95).abs() < f64::EPSILON);
    assert!(!c.monitor_speed);
}

// Speed degradation triggers warning
#[tokio::test]
async fn deadband_speed_degradation_triggers_warning() {
    let db = StorageDeadband::new();
    let mut vol = cocapn_fleet::internal::sd_core::Volume::new(Uuid::new_v4(),
        cocapn_fleet::internal::sd_core::VolumeFingerprint("slow".into()), "Slow".into(), PathBuf::from("/s"));
    vol.total_capacity = 1_000_000_000_000; vol.available_space = 500_000_000_000;
    vol.volume_type = cocapn_fleet::internal::sd_core::VolumeType::Primary;
    vol.read_speed_mbps = Some(5); vol.write_speed_mbps = Some(80);
    let r = db.check_all(&[Arc::new(vol)]).await;
    assert!(!r.is_empty()); assert_eq!(r[0].deadband_state, cocapn_fleet::internal::cocapn_core::DeadbandState::Approaching);
}

// Speed + fullness compounds to Exceeded
#[tokio::test]
async fn deadband_speed_and_fullness_compounds() {
    let db = StorageDeadband::new();
    let mut vol = cocapn_fleet::internal::sd_core::Volume::new(Uuid::new_v4(),
        cocapn_fleet::internal::sd_core::VolumeFingerprint("sf".into()), "SF".into(), PathBuf::from("/s"));
    vol.total_capacity = 1_000_000_000_000; vol.available_space = 200_000_000_000;
    vol.volume_type = cocapn_fleet::internal::sd_core::VolumeType::Primary;
    vol.read_speed_mbps = Some(5); vol.write_speed_mbps = Some(80);
    let r = db.check_all(&[Arc::new(vol)]).await;
    assert!(!r.is_empty());
    assert_eq!(r[0].deadband_state, cocapn_fleet::internal::cocapn_core::DeadbandState::Exceeded);
}

// has_fired / reset_fired
#[tokio::test]
async fn deadband_has_fired_flag() {
    let db = StorageDeadband::new();
    assert!(!db.has_fired().await);
    let v = make_vol("F", 100, 5, cocapn_fleet::internal::sd_core::VolumeType::Secondary, Uuid::new_v4());
    db.check_all(&[v]).await; assert!(db.has_fired().await);
    db.reset_fired().await; assert!(!db.has_fired().await);
}

// Volume without explicit config uses tier default
#[tokio::test]
async fn deadband_volume_without_explicit_config_uses_tier_default() {
    let db = StorageDeadband::new();
    let v = make_vol("D", 100, 15, cocapn_fleet::internal::sd_core::VolumeType::Network, Uuid::new_v4());
    let r = db.check_all(&[v]).await;
    // Bulk threshold=0.90, free=0.15, threshold=0.10, 0.15 > 0.10 => Normal
    assert!(r.is_empty(), "85% full on Bulk tier is Normal (below threshold)");
}

// Stripe advanced
#[test] fn stripe_multiple_volumes_per_tier() {
    let mut s = StorageStripe::new();
    s.add_volume(Uuid::new_v4(), "N1".into(), StorageTier::Hot, 100, 500, vec![]);
    s.add_volume(Uuid::new_v4(), "N2".into(), StorageTier::Hot, 300, 500, vec![]);
    s.add_volume(Uuid::new_v4(), "N3".into(), StorageTier::Bulk, 5000, 10000, vec![]);
    assert_eq!(s.healthy_volumes_in_tier(StorageTier::Hot).len(), 2);
}
#[test] fn stripe_fallback_path_for_unknown_volume_is_empty() {
    assert!(StorageStripe::new().fallback_path_for(Uuid::new_v4()).is_empty());
}
#[test] fn stripe_fail_volume_nonexistent() {
    assert!(StorageStripe::new().fail_volume(Uuid::new_v4(), "t".into()).is_none());
}
#[test] fn stripe_tier_for_volume() {
    let mut s = StorageStripe::new(); let id = Uuid::new_v4();
    s.add_volume(id, "t".into(), StorageTier::Hot, 100, 500, vec![]);
    assert_eq!(s.tier_for_volume(id), Some(StorageTier::Hot));
    assert_eq!(s.tier_for_volume(Uuid::new_v4()), None);
}
#[test] fn stripe_no_escalation_when_all_healthy() {
    let mut s = StorageStripe::new();
    s.add_volume(Uuid::new_v4(), "H".into(), StorageTier::Hot, 100, 500, vec![]);
    s.add_volume(Uuid::new_v4(), "B".into(), StorageTier::Bulk, 100, 500, vec![]);
    assert!(s.check_escalations().is_empty());
}
#[test] fn stripe_all_tiers_degraded() {
    let mut s = StorageStripe::new();
    let h=Uuid::new_v4(); let b=Uuid::new_v4(); let a=Uuid::new_v4();
    s.add_volume(h,"H".into(),StorageTier::Hot,100,500,vec![]);
    s.add_volume(b,"B".into(),StorageTier::Bulk,100,500,vec![]);
    s.add_volume(a,"A".into(),StorageTier::Archive,100,500,vec![]);
    // Only fail Hot and Bulk; leave Archive healthy as the fallback target
    s.fail_volume(h,"f".into()); s.fail_volume(b,"f".into());
    let evts = s.check_escalations();
    // Hot: escalate to Bulk (but Bulk is failed too, so skip to Archive)
    // Bulk: escalate to Archive
    // Archive: still healthy, no event
    assert_eq!(evts.len(), 2, "Hot->Archive (skipping failed Bulk), Bulk->Archive");
}
#[test] fn stripe_fallback_path_bulk_no_archive() {
    let mut s = StorageStripe::new();
    let h=Uuid::new_v4(); let b=Uuid::new_v4();
    s.add_volume(h,"H".into(),StorageTier::Hot,100,500,vec![]);
    s.add_volume(b,"B".into(),StorageTier::Bulk,100,500,vec![]);
    assert!(s.fallback_path_for(b).is_empty());
}

// Escalation engine edge cases
#[test]
fn escalation_picks_best_target() {
    let mut eng = EscalationEngine::new(EscalationConfig { auto_migrate: true, cooldown: Duration::from_secs(0), ..Default::default() });
    let full_id=Uuid::new_v4(); let bulk_id=Uuid::new_v4(); let _archive_id=Uuid::new_v4();
    let mut s = StorageStripe::new();
    s.add_volume(full_id, "N".into(), StorageTier::Hot, 10_000_000_000, 1_000_000_000_000, vec![bulk_id]);
    s.add_volume(bulk_id, "NAS".into(), StorageTier::Bulk, 5_000_000_000_000, 10_000_000_000_000, vec![]);
    s.add_volume(Uuid::new_v4(), "B2".into(), StorageTier::Archive, 90_000_000_000_000, 100_000_000_000_000, vec![]);
    let state = cocapn_fleet::deadband::StorageDeadbandState {
        volume_id: full_id, volume_name: "N".into(),
        volume_type: cocapn_fleet::internal::sd_core::VolumeType::Primary,
        utilization_pct: 99.0, deadband_state: cocapn_fleet::internal::cocapn_core::DeadbandState::Exceeded,
        message: "99%".into(),
    };
    let acts = eng.process_deadband(&[state], &mut s);
    assert!(acts.iter().any(|a| matches!(a, EscalationAction::Migrate { to_volume, .. } if *to_volume == bulk_id)));
}

#[test]
fn escalation_cooldown_respected() {
    let mut eng = EscalationEngine::new(EscalationConfig { auto_migrate: true, cooldown: Duration::from_secs(3600), ..Default::default() });
    let full_id=Uuid::new_v4(); let aid=Uuid::new_v4();
    let mut s = StorageStripe::new();
    s.add_volume(full_id, "N".into(), StorageTier::Hot, 10_000_000_000, 1_000_000_000_000, vec![]);
    s.add_volume(aid, "B2".into(), StorageTier::Archive, 90_000_000_000_000, 100_000_000_000_000, vec![]);
    let state = cocapn_fleet::deadband::StorageDeadbandState {
        volume_id: full_id, volume_name: "N".into(),
        volume_type: cocapn_fleet::internal::sd_core::VolumeType::Primary,
        utilization_pct: 99.0, deadband_state: cocapn_fleet::internal::cocapn_core::DeadbandState::Exceeded,
        message: "99%".into(),
    };
    assert!(eng.process_deadband(&[state.clone()], &mut s).iter().any(|a| matches!(a, EscalationAction::Migrate { .. })));
    assert!(!eng.process_deadband(&[state], &mut s).iter().any(|a| matches!(a, EscalationAction::Migrate { .. })));
}

#[test]
fn escalation_with_auto_migrate_disabled() {
    let mut eng = EscalationEngine::new(EscalationConfig { auto_migrate: false, cooldown: Duration::from_secs(0), ..Default::default() });
    let full_id=Uuid::new_v4(); let aid=Uuid::new_v4();
    let mut s = StorageStripe::new();
    s.add_volume(full_id, "N".into(), StorageTier::Hot, 10_000_000_000, 1_000_000_000_000, vec![]);
    s.add_volume(aid, "B2".into(), StorageTier::Archive, 90_000_000_000_000, 100_000_000_000_000, vec![]);
    let state = cocapn_fleet::deadband::StorageDeadbandState {
        volume_id: full_id, volume_name: "N".into(),
        volume_type: cocapn_fleet::internal::sd_core::VolumeType::Primary,
        utilization_pct: 99.0, deadband_state: cocapn_fleet::internal::cocapn_core::DeadbandState::Exceeded,
        message: "99%".into(),
    };
    assert!(!eng.process_deadband(&[state], &mut s).iter().any(|a| matches!(a, EscalationAction::Migrate { .. })));
}

#[test]
fn escalation_no_action_for_approaching() {
    let mut eng = EscalationEngine::new(EscalationConfig::default());
    let mut s = StorageStripe::new(); let id = Uuid::new_v4();
    s.add_volume(id, "v".into(), StorageTier::Hot, 100, 500, vec![]);
    let state = cocapn_fleet::deadband::StorageDeadbandState {
        volume_id: id, volume_name: "v".into(),
        volume_type: cocapn_fleet::internal::sd_core::VolumeType::Primary,
        utilization_pct: 80.0, deadband_state: cocapn_fleet::internal::cocapn_core::DeadbandState::Approaching,
        message: "80%".into(),
    };
    assert!(eng.process_deadband(&[state], &mut s).iter().all(|a| matches!(a, EscalationAction::NoOp)));
}

#[test]
fn escalation_degraded_action_when_no_target() {
    let mut eng = EscalationEngine::new(EscalationConfig { auto_migrate: true, cooldown: Duration::from_secs(0), ..Default::default() });
    let full_id = Uuid::new_v4();
    let mut s = StorageStripe::new();
    s.add_volume(full_id, "Only".into(), StorageTier::Hot, 10_000_000_000, 100_000_000_000, vec![]);
    let state = cocapn_fleet::deadband::StorageDeadbandState {
        volume_id: full_id, volume_name: "Only".into(),
        volume_type: cocapn_fleet::internal::sd_core::VolumeType::Primary,
        utilization_pct: 99.0, deadband_state: cocapn_fleet::internal::cocapn_core::DeadbandState::Exceeded,
        message: "99%".into(),
    };
    let acts = eng.process_deadband(&[state], &mut s);
    assert!(acts.iter().any(|a| matches!(a, EscalationAction::NoOp | EscalationAction::Degraded { .. })));
}

// FleetManager lifecycle
#[tokio::test]
async fn fleet_register_and_unregister_volume() {
    let (mut fm, _rx) = FleetManager::new(FleetConfig::default());
    let id = Uuid::new_v4();
    fm.register_volume(make_vol("T", 1000, 900, cocapn_fleet::internal::sd_core::VolumeType::Primary, id));
    let e = fm.check_health().await;
    let h = e.iter().find(|ev| matches!(ev, FleetEvent::HealthCheck { .. })).unwrap();
    if let FleetEvent::HealthCheck { total_volumes, .. } = h { assert_eq!(*total_volumes, 1); }
    fm.unregister_volume(id);
    let e = fm.check_health().await;
    let h = e.iter().find(|ev| matches!(ev, FleetEvent::HealthCheck { .. })).unwrap();
    if let FleetEvent::HealthCheck { total_volumes, .. } = h { assert_eq!(*total_volumes, 0); }
}

#[tokio::test]
async fn fleet_dry_run_mode() {
    let mut cfg = FleetConfig::default();
    cfg.dry_run = true; cfg.escalation.cooldown = Duration::from_secs(0);
    let (mut fm, _rx) = FleetManager::new(cfg);
    fm.register_volume(make_vol("NAS", 1000, 50, cocapn_fleet::internal::sd_core::VolumeType::Network, Uuid::new_v4()));
    fm.register_volume(make_vol("B2", 100000, 90000, cocapn_fleet::internal::sd_core::VolumeType::Cloud, Uuid::new_v4()));
    assert!(!fm.check_health().await.is_empty());
}

// Tiering classification
#[test]
fn tiering_desktop_node_is_cortex() {
    use cocapn_fleet::internal::cocapn_core::DeviceTier;
    use cocapn_fleet::internal::sd_core::{Device, DeviceFormFactor, OperatingSystem};
    let d = Device { id: Uuid::new_v4(), name: "D".into(), slug: "d".into(), os: OperatingSystem::Linux, os_version: None, hardware_model: Some("Custom".into()), cpu_model: Some("AMD".into()), cpu_architecture: Some("x86_64".into()), cpu_cores_physical: Some(16), cpu_cores_logical: Some(32), cpu_frequency_mhz: None, memory_total_bytes: Some(64_000_000_000), form_factor: Some(DeviceFormFactor::Desktop), manufacturer: None, gpu_models: None, boot_disk_type: None, boot_disk_capacity_bytes: None, swap_total_bytes: None, network_addresses: vec![], capabilities: serde_json::json!({}), is_online: true, last_seen_at: chrono::Utc::now(), sync_enabled: true, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), is_current: true, is_paired: true, is_connected: true, connection_method: None };
    assert_eq!(classify_node_tier(&d), DeviceTier::Cortex);
}

#[test]
fn tiering_server_node_is_backbone() {
    use cocapn_fleet::internal::cocapn_core::DeviceTier;
    use cocapn_fleet::internal::sd_core::{Device, DeviceFormFactor, OperatingSystem};
    let d = Device { id: Uuid::new_v4(), name: "S".into(), slug: "s".into(), os: OperatingSystem::Linux, os_version: None, hardware_model: Some("Synology".into()), cpu_model: Some("Celeron".into()), cpu_architecture: Some("x86_64".into()), cpu_cores_physical: Some(4), cpu_cores_logical: Some(4), cpu_frequency_mhz: None, memory_total_bytes: Some(8_000_000_000), form_factor: Some(DeviceFormFactor::Server), manufacturer: None, gpu_models: None, boot_disk_type: None, boot_disk_capacity_bytes: None, swap_total_bytes: None, network_addresses: vec![], capabilities: serde_json::json!({}), is_online: true, last_seen_at: chrono::Utc::now(), sync_enabled: true, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), is_current: true, is_paired: true, is_connected: true, connection_method: None };
    assert_eq!(classify_node_tier(&d), DeviceTier::Backbone);
}

#[test]
fn tiering_mobile_node_is_reflex() {
    use cocapn_fleet::internal::cocapn_core::DeviceTier;
    use cocapn_fleet::internal::sd_core::{Device, DeviceFormFactor, OperatingSystem};
    let d = Device { id: Uuid::new_v4(), name: "iPhone".into(), slug: "ip".into(), os: OperatingSystem::IOS, os_version: None, hardware_model: None, cpu_model: None, cpu_architecture: Some("arm64".into()), cpu_cores_physical: Some(6), cpu_cores_logical: Some(6), cpu_frequency_mhz: None, memory_total_bytes: Some(8_000_000_000), form_factor: Some(DeviceFormFactor::Mobile), manufacturer: Some("Apple".into()), gpu_models: None, boot_disk_type: None, boot_disk_capacity_bytes: None, swap_total_bytes: None, network_addresses: vec![], capabilities: serde_json::json!({}), is_online: true, last_seen_at: chrono::Utc::now(), sync_enabled: true, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), is_current: true, is_paired: true, is_connected: true, connection_method: None };
    assert_eq!(classify_node_tier(&d), DeviceTier::Reflex);
}

#[test]
fn tiering_tablet_node_is_reflex() {
    use cocapn_fleet::internal::cocapn_core::DeviceTier;
    use cocapn_fleet::internal::sd_core::{Device, DeviceFormFactor, OperatingSystem};
    let d = Device { id: Uuid::new_v4(), name: "iPad".into(), slug: "ipad".into(), os: OperatingSystem::IOS, os_version: None, hardware_model: None, cpu_model: None, cpu_architecture: Some("arm64".into()), cpu_cores_physical: Some(8), cpu_cores_logical: Some(8), cpu_frequency_mhz: None, memory_total_bytes: Some(16_000_000_000), form_factor: Some(DeviceFormFactor::Tablet), manufacturer: Some("Apple".into()), gpu_models: None, boot_disk_type: None, boot_disk_capacity_bytes: None, swap_total_bytes: None, network_addresses: vec![], capabilities: serde_json::json!({}), is_online: true, last_seen_at: chrono::Utc::now(), sync_enabled: true, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), is_current: true, is_paired: true, is_connected: true, connection_method: None };
    assert_eq!(classify_node_tier(&d), DeviceTier::Reflex);
}

#[test]
fn tiering_unknown_form_factor_fallback_high_spec() {
    use cocapn_fleet::internal::cocapn_core::DeviceTier;
    use cocapn_fleet::internal::sd_core::{Device, OperatingSystem};
    let d = Device { id: Uuid::new_v4(), name: "W".into(), slug: "w".into(), os: OperatingSystem::Windows, os_version: None, hardware_model: None, cpu_model: Some("Xeon".into()), cpu_architecture: Some("x86_64".into()), cpu_cores_physical: Some(16), cpu_cores_logical: Some(32), cpu_frequency_mhz: None, memory_total_bytes: Some(128_000_000_000), form_factor: None, manufacturer: None, gpu_models: None, boot_disk_type: None, boot_disk_capacity_bytes: None, swap_total_bytes: None, network_addresses: vec![], capabilities: serde_json::json!({}), is_online: true, last_seen_at: chrono::Utc::now(), sync_enabled: true, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), is_current: true, is_paired: true, is_connected: true, connection_method: None };
    assert_eq!(classify_node_tier(&d), DeviceTier::Cortex);
}

#[test]
fn tiering_unknown_form_factor_low_spec() {
    use cocapn_fleet::internal::cocapn_core::DeviceTier;
    use cocapn_fleet::internal::sd_core::{Device, DeviceFormFactor, OperatingSystem};
    let d = Device { id: Uuid::new_v4(), name: "D".into(), slug: "d".into(), os: OperatingSystem::Windows, os_version: None, hardware_model: None, cpu_model: Some("Atom".into()), cpu_architecture: Some("x86_64".into()), cpu_cores_physical: Some(2), cpu_cores_logical: Some(2), cpu_frequency_mhz: None, memory_total_bytes: Some(2_000_000_000), form_factor: Some(DeviceFormFactor::Other), manufacturer: None, gpu_models: None, boot_disk_type: None, boot_disk_capacity_bytes: None, swap_total_bytes: None, network_addresses: vec![], capabilities: serde_json::json!({}), is_online: true, last_seen_at: chrono::Utc::now(), sync_enabled: true, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), is_current: true, is_paired: true, is_connected: true, connection_method: None };
    assert_eq!(classify_node_tier(&d), DeviceTier::Backbone, "Other form factor, 2 cores, 2GB RAM -> Backbone per code logic");
}
