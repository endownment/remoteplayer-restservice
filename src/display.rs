//! Connected displays from the kernel DRM sysfs tree plus the live mode from mpv.
//!
//! sysfs knows which modes a connector supports and the monitor's EDID, but not
//! which mode is active; mpv owns the DRM device and reports that one.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::mpv::{MpvClient, MpvError};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct DisplayInfo {
    pub connectors: Vec<ConnectorInfo>,
    pub active_connector: Option<String>,
    pub current_mode: Option<CurrentMode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConnectorInfo {
    pub name: String,
    pub card: String,
    pub connected: bool,
    pub enabled: Option<bool>,
    pub preferred_mode: Option<String>,
    pub modes: Vec<String>,
    pub monitor: Option<MonitorIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct MonitorIdentity {
    pub manufacturer: String,
    pub product_code: u16,
    pub name: Option<String>,
    pub manufacture_year: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CurrentMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: Option<f64>,
}

pub async fn describe(
    sysfs_root: &Path,
    preferred_connector: Option<&str>,
    client: &MpvClient,
) -> Result<DisplayInfo, MpvError> {
    let root: PathBuf = sysfs_root.to_path_buf();
    let connectors: Vec<ConnectorInfo> =
        tokio::task::spawn_blocking(move || read_connectors(&root)).await.unwrap_or_default();
    let active_connector: Option<String> = choose_active(&connectors, preferred_connector);
    let current_mode: Option<CurrentMode> =
        if client.is_connected() { query_current_mode(client).await? } else { None };
    Ok(DisplayInfo { connectors, active_connector, current_mode })
}

async fn query_current_mode(client: &MpvClient) -> Result<Option<CurrentMode>, MpvError> {
    let width: Option<u32> = client.get_property_optional("display-width").await?;
    let height: Option<u32> = client.get_property_optional("display-height").await?;
    let refresh_hz: Option<f64> = client.get_property_optional("display-fps").await?;
    Ok(match (width, height) {
        (Some(width), Some(height)) => Some(CurrentMode { width, height, refresh_hz }),
        _ => None,
    })
}

fn choose_active(connectors: &[ConnectorInfo], preferred: Option<&str>) -> Option<String> {
    let by_preference: Option<&ConnectorInfo> = preferred.and_then(|name: &str| {
        connectors.iter().filter(is_connected).find(|connector: &&ConnectorInfo| connector.name == name)
    });
    by_preference
        .or_else(|| connectors.iter().find(is_connected))
        .map(|connector: &ConnectorInfo| connector.name.clone())
}

fn is_connected(connector: &&ConnectorInfo) -> bool {
    connector.connected
}

/// Entries look like `card0-HDMI-A-1`; plain `card0` and `renderD128` are devices, not connectors.
pub fn read_connectors(sysfs_root: &Path) -> Vec<ConnectorInfo> {
    let Ok(entries): std::io::Result<std::fs::ReadDir> = std::fs::read_dir(sysfs_root) else {
        return Vec::new();
    };
    let mut connectors: Vec<ConnectorInfo> = entries
        .filter_map(|entry: std::io::Result<std::fs::DirEntry>| entry.ok())
        .filter_map(|entry: std::fs::DirEntry| {
            let directory_name: String = entry.file_name().to_str()?.to_string();
            let (card, connector): (&str, &str) = directory_name.split_once('-')?;
            if !card.starts_with("card") {
                return None;
            }
            Some(read_connector(&entry.path(), card, connector))
        })
        .collect();
    connectors.sort_by(|left: &ConnectorInfo, right: &ConnectorInfo| {
        (&left.card, &left.name).cmp(&(&right.card, &right.name))
    });
    connectors
}

fn read_connector(directory: &Path, card: &str, connector: &str) -> ConnectorInfo {
    let status: String = read_trimmed(&directory.join("status")).unwrap_or_default();
    let enabled: Option<bool> = read_trimmed(&directory.join("enabled")).map(|value: String| value == "enabled");
    let mut modes: Vec<String> = Vec::new();
    for mode in read_trimmed(&directory.join("modes")).unwrap_or_default().lines() {
        if !modes.iter().any(|known: &String| known == mode) {
            modes.push(mode.to_string());
        }
    }
    let monitor: Option<MonitorIdentity> =
        std::fs::read(directory.join("edid")).ok().and_then(|edid: Vec<u8>| parse_edid(&edid));
    ConnectorInfo {
        name: connector.to_string(),
        card: card.to_string(),
        connected: status == "connected",
        enabled,
        // The kernel lists the monitor's preferred mode first.
        preferred_mode: modes.first().cloned(),
        modes,
        monitor,
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|text: String| text.trim().to_string())
}

const EDID_HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
const EDID_BLOCK_SIZE: usize = 128;
const DESCRIPTOR_OFFSETS: [usize; 4] = [54, 72, 90, 108];
const MONITOR_NAME_TAG: u8 = 0xFC;

/// Reads the identification fields of the EDID base block (VESA E-EDID 1.4, section 3).
pub fn parse_edid(edid: &[u8]) -> Option<MonitorIdentity> {
    let block: &[u8] = edid.get(..EDID_BLOCK_SIZE)?;
    if block[..8] != EDID_HEADER {
        return None;
    }
    let checksum: u8 = block.iter().fold(0u8, |sum: u8, byte: &u8| sum.wrapping_add(*byte));
    if checksum != 0 {
        return None;
    }
    // Three 5-bit letters, 'A' = 1, packed big-endian into bytes 8..10.
    let packed: u16 = u16::from_be_bytes([block[8], block[9]]);
    let manufacturer: String = [10u16, 5, 0].iter().map(|shift: &u16| edid_letter(packed, *shift)).collect();
    let product_code: u16 = u16::from_le_bytes([block[10], block[11]]);
    let manufacture_year: Option<u16> = (block[17] != 0).then(|| 1990 + u16::from(block[17]));
    let name: Option<String> = DESCRIPTOR_OFFSETS.iter().find_map(|offset: &usize| {
        let descriptor: &[u8] = &block[*offset..*offset + 18];
        let is_name: bool = descriptor[..3] == [0, 0, 0] && descriptor[3] == MONITOR_NAME_TAG;
        is_name.then(|| {
            let text: &[u8] = &descriptor[5..18];
            let end: usize = text.iter().position(|byte: &u8| *byte == 0x0A).unwrap_or(text.len());
            String::from_utf8_lossy(&text[..end]).trim().to_string()
        })
    });
    Some(MonitorIdentity { manufacturer, product_code, name, manufacture_year })
}

fn edid_letter(packed: u16, shift: u16) -> char {
    let index: u8 = ((packed >> shift) & 0x1F) as u8;
    char::from(b'A' - 1 + index)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A synthetic but valid EDID base block: manufacturer "RPT", product 0x1234,
    /// year 2024, monitor name "Living Room".
    pub fn sample_edid() -> Vec<u8> {
        let mut block: Vec<u8> = vec![0u8; EDID_BLOCK_SIZE];
        block[..8].copy_from_slice(&EDID_HEADER);
        let letters: [u8; 3] = [b'R' - b'A' + 1, b'P' - b'A' + 1, b'T' - b'A' + 1];
        let packed: u16 = (u16::from(letters[0]) << 10) | (u16::from(letters[1]) << 5) | u16::from(letters[2]);
        block[8..10].copy_from_slice(&packed.to_be_bytes());
        block[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        block[17] = 34;
        block[18] = 1;
        block[19] = 4;
        let descriptor: &mut [u8] = &mut block[54..72];
        descriptor[3] = MONITOR_NAME_TAG;
        let name: &[u8] = b"Living Room\n ";
        descriptor[5..5 + name.len()].copy_from_slice(name);
        let sum: u8 = block[..127].iter().fold(0u8, |sum: u8, byte: &u8| sum.wrapping_add(*byte));
        block[127] = 0u8.wrapping_sub(sum);
        block
    }

    #[test]
    fn edid_identity_is_decoded() {
        let identity: MonitorIdentity = parse_edid(&sample_edid()).expect("valid edid");
        assert_eq!(
            identity,
            MonitorIdentity {
                manufacturer: "RPT".to_string(),
                product_code: 0x1234,
                name: Some("Living Room".to_string()),
                manufacture_year: Some(2024),
            }
        );
    }

    #[test]
    fn corrupted_edid_is_ignored() {
        let mut edid: Vec<u8> = sample_edid();
        edid[20] ^= 0xFF;
        assert_eq!(parse_edid(&edid), None);
        assert_eq!(parse_edid(&[]), None);
    }

    #[test]
    fn connectors_are_read_from_a_fake_sysfs_tree() {
        let root: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let hdmi: PathBuf = root.path().join("card1-HDMI-A-1");
        std::fs::create_dir_all(&hdmi).expect("hdmi dir");
        std::fs::write(hdmi.join("status"), "connected\n").expect("status");
        std::fs::write(hdmi.join("enabled"), "enabled\n").expect("enabled");
        std::fs::write(hdmi.join("modes"), "1920x1080\n1920x1080\n1280x720\n").expect("modes");
        std::fs::write(hdmi.join("edid"), sample_edid()).expect("edid");
        let composite: PathBuf = root.path().join("card1-Composite-1");
        std::fs::create_dir_all(&composite).expect("composite dir");
        std::fs::write(composite.join("status"), "disconnected\n").expect("status");
        std::fs::create_dir_all(root.path().join("card1")).expect("device dir");

        let connectors: Vec<ConnectorInfo> = read_connectors(root.path());
        assert_eq!(connectors.len(), 2);
        let hdmi_info: &ConnectorInfo =
            connectors.iter().find(|connector: &&ConnectorInfo| connector.name == "HDMI-A-1").expect("hdmi");
        assert!(hdmi_info.connected);
        assert_eq!(hdmi_info.modes, vec!["1920x1080".to_string(), "1280x720".to_string()]);
        assert_eq!(hdmi_info.preferred_mode.as_deref(), Some("1920x1080"));
        assert_eq!(choose_active(&connectors, None).as_deref(), Some("HDMI-A-1"));
    }
}
