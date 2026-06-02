//! ## Tiering: Classify Spacedrive nodes & volumes into CoCapn tiers
//!
//! ### Node roles
//!
//! | Spacedrive Node | CoCapn Tier | Rationale |
//! |-----------------|-------------|-----------|
//! | NAS / home server | Backbone | Always-on, moderate CPU, bulk storage |
//! | Desktop (Linux/Win) | Cortex | Fast NVMe, GPU-capable, working set |
//! | Laptop | Cortex | Mobile compute, fast local SSD |
//! | Cloud volume (S3, B2, GCS) | Cloud | API-driven, abundant capacity |
//! | Mobile phone | Reflex | Intermittent, limited storage |
//!
//! ### Volume storage tiers
//!
//! | VolumeType | StorageTier | Role |
//! |------------|-------------|------|
//! | Primary (SSD) | Hot | Working set, fast I/O |
//! | Secondary / NAS | Bulk | High capacity, moderate speed |
//! | Cloud | Archive | Infinite capacity, slower access |
//! | External | Offload | Disconnectable cold storage |

use crate::internal::cocapn_core::DeviceTier;
use crate::internal::sd_core::{Device, DeviceFormFactor, VolumeType};

/// A storage tier — the volume analogue of CoCapn's DeviceTier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageTier {
    /// Hot working set (primary SSD) — fast, low capacity. ≈ CoCapn Cortex.
    Hot,
    /// Bulk storage (NAS, secondary drives) — high capacity, moderate speed. ≈ CoCapn Backbone.
    Bulk,
    /// Archive (S3, B2, GCS, Glacier) — unlimited, slow. ≈ CoCapn Cloud.
    Archive,
    /// Offload (external, removable) — disconnectedle cold. ≈ CoCapn Reflex.
    Offload,
}

impl StorageTier {
    pub fn display_name(&self) -> &'static str {
        match self {
            StorageTier::Hot => "Hot (working set)",
            StorageTier::Bulk => "Bulk (NAS / secondary)",
            StorageTier::Archive => "Archive (cloud)",
            StorageTier::Offload => "Offload (external)",
        }
    }
}

/// CoCapn role assigned to a Spacedrive node.
#[derive(Debug, Clone)]
pub struct CoCapnNodeRole {
    pub device_id: uuid::Uuid,
    pub device_name: String,
    pub tier: DeviceTier,
    pub storage_tiers: Vec<StorageTier>,
}

/// Classify a Spacedrive `Device` into a CoCapn `DeviceTier`.
pub fn classify_node_tier(device: &Device) -> DeviceTier {
    match device.form_factor {
        Some(DeviceFormFactor::Server) => {
            // Server-class machines: NAS, rackmount → Backbone
            DeviceTier::Backbone
        }
        Some(DeviceFormFactor::Desktop) => {
            // Desktops (GPU-capable, fast NVMe) → Cortex
            DeviceTier::Cortex
        }
        Some(DeviceFormFactor::Laptop) => {
            // Laptops → Cortex (mobile compute)
            DeviceTier::Cortex
        }
        Some(DeviceFormFactor::Mobile) | Some(DeviceFormFactor::Tablet) => {
            // Phones/tablets → Reflex
            DeviceTier::Reflex
        }
        Some(DeviceFormFactor::Other) | None => {
            // Fallback: check hardware clues
            let cpu_cores = device.cpu_cores_logical.unwrap_or(0);
            let mem_gb = device
                .memory_total_bytes
                .map(|b| b as f64 / 1_000_000_000.0)
                .unwrap_or(0.0);

            if cpu_cores >= 8 && mem_gb >= 8.0 {
                DeviceTier::Cortex
            } else if cpu_cores >= 2 && mem_gb >= 1.0 {
                DeviceTier::Backbone
            } else {
                DeviceTier::Reflex
            }
        }
    }
}

/// Classify a Spacedrive `VolumeType` into a `StorageTier`.
pub fn classify_volume_tier(volume_type: VolumeType) -> StorageTier {
    match volume_type {
        VolumeType::Primary | VolumeType::UserData => StorageTier::Hot,
        VolumeType::Secondary | VolumeType::Network => StorageTier::Bulk,
        VolumeType::Cloud => StorageTier::Archive,
        VolumeType::External => StorageTier::Offload,
        VolumeType::System => StorageTier::Hot,   // system volumes are fast
        VolumeType::Virtual | VolumeType::Unknown => StorageTier::Bulk,
    }
}

impl From<StorageTier> for DeviceTier {
    /// Map StorageTier back to DeviceTier for stripe/fallback compatibility.
    fn from(st: StorageTier) -> Self {
        match st {
            StorageTier::Hot => DeviceTier::Cortex,
            StorageTier::Bulk => DeviceTier::Backbone,
            StorageTier::Archive => DeviceTier::Cloud,
            StorageTier::Offload => DeviceTier::Reflex,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::internal::sd_core::{DeviceFormFactor, OperatingSystem};
    use uuid::Uuid;

    fn laptop_device() -> Device {
        Device {
            id: Uuid::new_v4(),
            name: "MacBook Pro".into(),
            slug: "macbook-pro".into(),
            os: OperatingSystem::MacOS,
            os_version: Some("14.5".into()),
            hardware_model: Some("MacBookPro18,3".into()),
            cpu_model: Some("Apple M3 Max".into()),
            cpu_architecture: Some("arm64".into()),
            cpu_cores_physical: Some(12),
            cpu_cores_logical: Some(12),
            cpu_frequency_mhz: None,
            memory_total_bytes: Some(36_000_000_000),
            form_factor: Some(DeviceFormFactor::Laptop),
            manufacturer: Some("Apple".into()),
            gpu_models: Some(vec!["Apple M3 Max (40-core)".into()]),
            boot_disk_type: Some("NVMe".into()),
            boot_disk_capacity_bytes: Some(1_000_000_000_000),
            swap_total_bytes: None,
            network_addresses: vec![],
            capabilities: serde_json::json!({}),
            is_online: true,
            last_seen_at: Utc::now(),
            sync_enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_current: true,
            is_paired: true,
            is_connected: true,
            connection_method: None,
        }
    }

    #[test]
    fn laptop_is_cortex() {
        let dev = laptop_device();
        assert_eq!(classify_node_tier(&dev), DeviceTier::Cortex);
    }

    #[test]
    fn volume_types_map_correctly() {
        assert_eq!(classify_volume_tier(VolumeType::Primary), StorageTier::Hot);
        assert_eq!(classify_volume_tier(VolumeType::Network), StorageTier::Bulk);
        assert_eq!(classify_volume_tier(VolumeType::Cloud), StorageTier::Archive);
        assert_eq!(
            classify_volume_tier(VolumeType::External),
            StorageTier::Offload
        );
    }

    #[test]
    fn storage_tier_device_tier_roundtrip() {
        assert_eq!(
            DeviceTier::from(StorageTier::Hot),
            DeviceTier::Cortex
        );
        assert_eq!(
            DeviceTier::from(StorageTier::Archive),
            DeviceTier::Cloud
        );
    }
}
