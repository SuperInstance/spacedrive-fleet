//! Stub types for Spacedrive core domain models.
//!
//! In-production FleetManager links against Spacedrive's actual `sd-core` crate.
//! This stand-in reproduces the minimal domain types needed to compile and test
//! the fleet logic independently.
//!
//! ## Volume
//!
//! A storage volume managed by Spacedrive — a mounted filesystem (local SSD,
//! network NAS share, cloud bucket, external drive).
//!
//! ## Device
//!
//! A node running Spacedrive — could be a NAS, desktop, laptop, phone, or tablet.

use std::path::PathBuf;
use uuid::Uuid;

/// What kind of volume is this?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumeType {
    /// Primary internal drive (SSD, NVMe).
    Primary,
    /// User data volume.
    UserData,
    /// Secondary drive (HDD, large storage).
    Secondary,
    /// Network volume (NAS, SMB, NFS).
    Network,
    /// Cloud volume (S3, B2, GCS).
    Cloud,
    /// External removable drive (USB, Thunderbolt).
    External,
    /// System volume (boot, recovery).
    System,
    /// Virtual or RAM volume.
    Virtual,
    /// Unknown or unmapped volume type.
    Unknown,
}

/// Unique fingerprint for a storage volume.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VolumeFingerprint(pub String);

/// A storage volume managed by Spacedrive.
///
/// The real `sd_core::domain::volume::Volume` has many more fields.
/// This stub covers what cocapn-fleet actually reads from it.
#[derive(Debug, Clone)]
pub struct Volume {
    pub id: Uuid,
    pub fingerprint: VolumeFingerprint,
    pub name: String,
    pub mount_path: PathBuf,
    pub total_capacity: u64,
    pub available_space: u64,
    pub volume_type: VolumeType,
    pub read_speed_mbps: Option<u64>,
    pub write_speed_mbps: Option<u64>,
}

impl Volume {
    pub fn new(
        id: Uuid,
        fingerprint: VolumeFingerprint,
        name: String,
        mount_path: PathBuf,
    ) -> Self {
        Self {
            id,
            fingerprint,
            name,
            mount_path,
            total_capacity: 0,
            available_space: 0,
            volume_type: VolumeType::Unknown,
            read_speed_mbps: None,
            write_speed_mbps: None,
        }
    }

    /// Display name for the volume.
    pub fn display_name(&self) -> &str {
        &self.name
    }

    /// Utilization as a percentage (0.0–100.0).
    pub fn utilization_percentage(&self) -> f64 {
        if self.total_capacity == 0 {
            return 0.0;
        }
        let used = self.total_capacity - self.available_space;
        (used as f64 / self.total_capacity as f64) * 100.0
    }
}

/// Device form factor — helps classify compute tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceFormFactor {
    Server,
    Desktop,
    Laptop,
    Mobile,
    Tablet,
    Other,
}

/// Operating system of the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatingSystem {
    Linux,
    MacOS,
    Windows,
    IOS,
    Android,
    Unknown,
}

/// A device running Spacedrive.
#[derive(Debug, Clone)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub os: OperatingSystem,
    pub os_version: Option<String>,
    pub hardware_model: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_architecture: Option<String>,
    pub cpu_cores_physical: Option<u64>,
    pub cpu_cores_logical: Option<u64>,
    pub cpu_frequency_mhz: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub form_factor: Option<DeviceFormFactor>,
    pub manufacturer: Option<String>,
    pub gpu_models: Option<Vec<String>>,
    pub boot_disk_type: Option<String>,
    pub boot_disk_capacity_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub network_addresses: Vec<String>,
    pub capabilities: serde_json::Value,
    pub is_online: bool,
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
    pub sync_enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub is_current: bool,
    pub is_paired: bool,
    pub is_connected: bool,
    pub connection_method: Option<String>,
}
