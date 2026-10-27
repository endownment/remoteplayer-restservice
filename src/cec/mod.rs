//! HDMI-CEC: who is on the HDMI bus and what state they are in.
//!
//! On a Raspberry Pi the vc4 HDMI driver exposes the CEC adapter as `/dev/cec0`
//! through the kernel CEC framework (`linux/cec.h`), which `linux.rs` talks to
//! with ioctl calls. `stub.rs` serves fixed data on machines without CEC.

mod linux;
mod stub;

use std::path::PathBuf;

use serde::Serialize;

use crate::config::CecConfig;

#[derive(Debug, thiserror::Error)]
pub enum CecError {
    #[error("CEC is disabled in the configuration")]
    Disabled,
    #[error("cannot open {device}: {source}")]
    Open { device: String, source: std::io::Error },
    #[error("CEC ioctl {operation} failed: {source}")]
    Ioctl { operation: &'static str, source: nix::errno::Errno },
    #[error("CEC worker thread failed: {0}")]
    Worker(String),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CecReport {
    pub adapter: CecAdapterInfo,
    pub devices: Vec<CecDeviceInfo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CecAdapterInfo {
    pub device: String,
    pub driver: String,
    pub name: String,
    pub physical_address: Option<String>,
    pub logical_addresses: Vec<u8>,
    pub claimed_now: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CecDeviceInfo {
    pub logical_address: u8,
    pub role: &'static str,
    pub physical_address: Option<String>,
    pub osd_name: Option<String>,
    pub vendor_id: Option<String>,
    pub vendor_name: Option<&'static str>,
    pub power_status: Option<CecPowerStatus>,
    pub cec_version: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CecPowerStatus {
    On,
    Standby,
    TransitioningToOn,
    TransitioningToStandby,
    Unknown,
}

impl CecPowerStatus {
    pub fn from_operand(operand: u8) -> CecPowerStatus {
        match operand {
            0 => CecPowerStatus::On,
            1 => CecPowerStatus::Standby,
            2 => CecPowerStatus::TransitioningToOn,
            3 => CecPowerStatus::TransitioningToStandby,
            _ => CecPowerStatus::Unknown,
        }
    }
}

/// A closed set of implementations, so an enum instead of a trait object:
/// the compiler checks that every `match` handles every backend, and no
/// heap allocation or virtual call is involved.
pub enum CecBackend {
    Disabled,
    Stub,
    Linux { device: PathBuf, claim_logical_address: bool, osd_name: String },
}

impl CecBackend {
    pub fn from_config(config: &CecConfig) -> CecBackend {
        match config {
            CecConfig::Disabled => CecBackend::Disabled,
            CecConfig::Stub => CecBackend::Stub,
            CecConfig::Linux { device, claim_logical_address, osd_name } => CecBackend::Linux {
                device: device.clone(),
                claim_logical_address: *claim_logical_address,
                osd_name: osd_name.clone(),
            },
        }
    }

    /// Polls all 15 logical addresses and asks every responding device for its
    /// identity and power state. Takes a few hundred milliseconds per device.
    pub async fn scan(&self) -> Result<CecReport, CecError> {
        match self {
            CecBackend::Disabled => Err(CecError::Disabled),
            CecBackend::Stub => Ok(stub::report()),
            CecBackend::Linux { device, claim_logical_address, osd_name } => {
                let device: PathBuf = device.clone();
                let claim: bool = *claim_logical_address;
                let osd_name: String = osd_name.clone();
                // The ioctls block (each query waits up to a second for a reply),
                // so they run on Tokio's blocking thread pool.
                tokio::task::spawn_blocking(move || linux::scan(&device, claim, &osd_name))
                    .await
                    .map_err(|error: tokio::task::JoinError| CecError::Worker(error.to_string()))?
            }
        }
    }
}

pub fn role_of(logical_address: u8) -> &'static str {
    match logical_address {
        0 => "tv",
        1 | 2 | 9 => "recording-device",
        3 | 6 | 7 | 10 => "tuner",
        4 | 8 | 11 => "playback-device",
        5 => "audio-system",
        12 | 13 => "backup",
        14 => "specific-use",
        _ => "unregistered",
    }
}

pub fn format_physical_address(address: u16) -> Option<String> {
    if address == 0xFFFF {
        return None;
    }
    Some(format!("{}.{}.{}.{}", address >> 12, (address >> 8) & 0xF, (address >> 4) & 0xF, address & 0xF))
}

pub fn cec_version_name(operand: u8) -> Option<&'static str> {
    match operand {
        0x04 => Some("1.3a"),
        0x05 => Some("1.4"),
        0x06 => Some("2.0"),
        _ => None,
    }
}

pub fn vendor_name(vendor_id: u32) -> Option<&'static str> {
    match vendor_id {
        0x0000F0 => Some("Samsung"),
        0x00E091 => Some("LG"),
        0x080046 => Some("Sony"),
        0x00903E => Some("Philips"),
        0x008045 => Some("Panasonic"),
        0x000039 => Some("Toshiba"),
        0x08001F => Some("Sharp"),
        0x0009B0 => Some("Onkyo"),
        0x00E036 => Some("Pioneer"),
        0x00A0DE => Some("Yamaha"),
        0x0005CD => Some("Denon"),
        0x001582 => Some("Pulse-Eight"),
        0x0018C0 => Some("Broadcom"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_address_uses_dotted_nibbles() {
        assert_eq!(format_physical_address(0x1000).as_deref(), Some("1.0.0.0"));
        assert_eq!(format_physical_address(0x2130).as_deref(), Some("2.1.3.0"));
        assert_eq!(format_physical_address(0xFFFF), None);
    }

    #[test]
    fn roles_follow_the_cec_address_table() {
        assert_eq!(role_of(0), "tv");
        assert_eq!(role_of(4), "playback-device");
        assert_eq!(role_of(5), "audio-system");
        assert_eq!(role_of(15), "unregistered");
    }
}
