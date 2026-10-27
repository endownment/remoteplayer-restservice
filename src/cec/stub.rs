use crate::cec::{CecAdapterInfo, CecDeviceInfo, CecPowerStatus, CecReport};

/// What a Raspberry Pi connected to a typical TV reports, for development
/// machines without a CEC adapter.
pub fn report() -> CecReport {
    CecReport {
        adapter: CecAdapterInfo {
            device: "stub".to_string(),
            driver: "stub".to_string(),
            name: "stub-cec".to_string(),
            physical_address: Some("1.0.0.0".to_string()),
            logical_addresses: vec![4],
            claimed_now: false,
        },
        devices: vec![CecDeviceInfo {
            logical_address: 0,
            role: "tv",
            physical_address: Some("0.0.0.0".to_string()),
            osd_name: Some("Stub TV".to_string()),
            vendor_id: Some("0x00e091".to_string()),
            vendor_name: Some("LG"),
            power_status: Some(CecPowerStatus::On),
            cec_version: Some("1.4"),
        }],
        warnings: vec!["CEC stub backend: data is fixed and not read from hardware".to_string()],
    }
}
