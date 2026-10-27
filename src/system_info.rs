//! Health of the board itself. On a Raspberry Pi 3 an under-powered supply makes
//! the firmware throttle the CPU, which shows up as stuttering video; the
//! `throttled` flags make that visible without guessing.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::config::SystemConfig;

const VCGENCMD_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SystemInfo {
    pub hostname: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub load_average: Option<[f64; 3]>,
    pub memory_total_bytes: Option<u64>,
    pub memory_available_bytes: Option<u64>,
    pub temperature_celsius: Option<f64>,
    pub throttled: Option<ThrottleState>,
}

/// Bits of the firmware's `get_throttled` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ThrottleState {
    pub raw: String,
    pub under_voltage_now: bool,
    pub frequency_capped_now: bool,
    pub throttled_now: bool,
    pub soft_temperature_limit_now: bool,
    pub under_voltage_occurred: bool,
    pub frequency_capped_occurred: bool,
    pub throttled_occurred: bool,
    pub soft_temperature_limit_occurred: bool,
}

impl ThrottleState {
    pub fn from_bits(bits: u32) -> ThrottleState {
        ThrottleState {
            raw: format!("0x{bits:x}"),
            under_voltage_now: is_bit_set(bits, 0),
            frequency_capped_now: is_bit_set(bits, 1),
            throttled_now: is_bit_set(bits, 2),
            soft_temperature_limit_now: is_bit_set(bits, 3),
            under_voltage_occurred: is_bit_set(bits, 16),
            frequency_capped_occurred: is_bit_set(bits, 17),
            throttled_occurred: is_bit_set(bits, 18),
            soft_temperature_limit_occurred: is_bit_set(bits, 19),
        }
    }
}

fn is_bit_set(bits: u32, index: u32) -> bool {
    bits & (1 << index) != 0
}

pub async fn collect(config: &SystemConfig) -> SystemInfo {
    let (memory_total_bytes, memory_available_bytes): (Option<u64>, Option<u64>) = read_memory();
    SystemInfo {
        hostname: nix::unistd::gethostname().ok().and_then(|name: std::ffi::OsString| name.into_string().ok()),
        uptime_seconds: read_first_number(Path::new("/proc/uptime")),
        load_average: read_load_average(),
        memory_total_bytes,
        memory_available_bytes,
        temperature_celsius: read_first_number(&config.thermal_zone_file)
            .map(|millidegrees: f64| millidegrees / 1000.0),
        throttled: read_throttled(config).await,
    }
}

fn read_first_number(path: &Path) -> Option<f64> {
    let text: String = std::fs::read_to_string(path).ok()?;
    text.split_whitespace().next()?.parse::<f64>().ok()
}

fn read_load_average() -> Option<[f64; 3]> {
    let text: String = std::fs::read_to_string("/proc/loadavg").ok()?;
    let values: Vec<f64> =
        text.split_whitespace().take(3).filter_map(|field: &str| field.parse::<f64>().ok()).collect();
    match values.as_slice() {
        [one, five, fifteen] => Some([*one, *five, *fifteen]),
        _ => None,
    }
}

fn read_memory() -> (Option<u64>, Option<u64>) {
    let Ok(text): std::io::Result<String> = std::fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    (meminfo_bytes(&text, "MemTotal:"), meminfo_bytes(&text, "MemAvailable:"))
}

fn meminfo_bytes(meminfo: &str, field: &str) -> Option<u64> {
    let line: &str = meminfo.lines().find(|line: &&str| line.starts_with(field))?;
    let kibibytes: u64 = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(kibibytes * 1024)
}

async fn read_throttled(config: &SystemConfig) -> Option<ThrottleState> {
    if let Some(bits) = std::fs::read_to_string(&config.throttled_file).ok().and_then(|text: String| parse_hex(&text)) {
        return Some(ThrottleState::from_bits(bits));
    }
    let output: std::process::Output = tokio::time::timeout(
        VCGENCMD_TIMEOUT,
        tokio::process::Command::new(&config.vcgencmd).arg("get_throttled").kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    let text: String = String::from_utf8_lossy(&output.stdout).to_string();
    parse_hex(text.trim().strip_prefix("throttled=")?).map(ThrottleState::from_bits)
}

fn parse_hex(text: &str) -> Option<u32> {
    let trimmed: &str = text.trim();
    let digits: &str = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    u32::from_str_radix(digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_bits_are_decoded() {
        let state: ThrottleState = ThrottleState::from_bits(parse_hex("0x50005").expect("hex"));
        assert!(state.under_voltage_now);
        assert!(state.throttled_now);
        assert!(!state.frequency_capped_now);
        assert!(state.under_voltage_occurred);
        assert!(state.throttled_occurred);
        assert_eq!(state.raw, "0x50005");
    }

    #[test]
    fn sysfs_value_without_prefix_is_accepted() {
        assert_eq!(parse_hex("50000\n"), Some(0x50000));
    }
}
