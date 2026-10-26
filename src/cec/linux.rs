//! Linux CEC framework client (`include/uapi/linux/cec.h`,
//! https://docs.kernel.org/userspace-api/media/cec/cec-api.html).
//!
//! This is the only module with `unsafe` code: ioctl passes raw pointers to
//! the kernel, and the compiler cannot check what the kernel does with them.
//! Every `unsafe` block states why it is sound in a `SAFETY:` comment, which is
//! the Rust community convention for auditing such code.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

use crate::cec::{
    CecAdapterInfo, CecDeviceInfo, CecError, CecPowerStatus, CecReport, cec_version_name, format_physical_address,
    role_of, vendor_name,
};

const CEC_MAX_MSG_SIZE: usize = 16;
const CEC_MAX_LOG_ADDRS: usize = 4;
const CEC_PHYS_ADDR_INVALID: u16 = 0xFFFF;
const BROADCAST_ADDRESS: u8 = 15;

const CEC_CAP_LOG_ADDRS: u32 = 1 << 1;
const CEC_TX_STATUS_OK: u8 = 1 << 0;
const CEC_RX_STATUS_OK: u8 = 1 << 0;

const CEC_LOG_ADDR_TYPE_PLAYBACK: u8 = 3;
const CEC_OP_PRIM_DEVTYPE_PLAYBACK: u8 = 4;
const CEC_OP_ALL_DEVTYPE_PLAYBACK: u8 = 0x10;
const CEC_OP_CEC_VERSION_1_4: u8 = 5;
const CEC_VENDOR_ID_NONE: u32 = 0xFFFF_FFFF;

const OPCODE_GIVE_PHYSICAL_ADDR: u8 = 0x83;
const OPCODE_REPORT_PHYSICAL_ADDR: u8 = 0x84;
const OPCODE_GIVE_OSD_NAME: u8 = 0x46;
const OPCODE_SET_OSD_NAME: u8 = 0x47;
const OPCODE_GIVE_DEVICE_VENDOR_ID: u8 = 0x8C;
const OPCODE_DEVICE_VENDOR_ID: u8 = 0x87;
const OPCODE_GIVE_DEVICE_POWER_STATUS: u8 = 0x8F;
const OPCODE_REPORT_POWER_STATUS: u8 = 0x90;
const OPCODE_GET_CEC_VERSION: u8 = 0x9F;
const OPCODE_CEC_VERSION: u8 = 0x9E;

const REPLY_TIMEOUT_MS: u32 = 1000;

// `#[repr(C)]` makes Rust lay the fields out exactly like the C compiler does
// (same order, same padding). Without it Rust may reorder fields.
// C `char` arrays are declared as `u8` arrays: same size and alignment, and it
// avoids `c_char` being signed on x86 but unsigned on ARM.

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CecCaps {
    driver: [u8; 32],
    name: [u8; 32],
    available_log_addrs: u32,
    capabilities: u32,
    version: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CecLogAddrs {
    log_addr: [u8; CEC_MAX_LOG_ADDRS],
    log_addr_mask: u16,
    cec_version: u8,
    num_log_addrs: u8,
    vendor_id: u32,
    flags: u32,
    osd_name: [u8; 15],
    primary_device_type: [u8; CEC_MAX_LOG_ADDRS],
    log_addr_type: [u8; CEC_MAX_LOG_ADDRS],
    all_device_types: [u8; CEC_MAX_LOG_ADDRS],
    features: [[u8; 12]; CEC_MAX_LOG_ADDRS],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CecMsg {
    tx_ts: u64,
    rx_ts: u64,
    len: u32,
    timeout: u32,
    sequence: u32,
    flags: u32,
    msg: [u8; CEC_MAX_MSG_SIZE],
    reply: u8,
    rx_status: u8,
    tx_status: u8,
    tx_arb_lost_cnt: u8,
    tx_nack_cnt: u8,
    tx_low_drive_cnt: u8,
    tx_error_cnt: u8,
}

// Compile-time check against the sizes the C declarations in linux/cec.h give on
// both 32-bit and 64-bit ARM (u64 fields are 8-byte aligned in the ARM EABI too).
// The ioctl numbers encode these sizes, so a mismatch fails the build instead of
// corrupting memory at run time.
const _: () = assert!(std::mem::size_of::<CecCaps>() == 76);
const _: () = assert!(std::mem::size_of::<CecLogAddrs>() == 92);
const _: () = assert!(std::mem::size_of::<CecMsg>() == 56);

// These macros generate `unsafe fn name(fd: c_int, data: *mut T) -> nix::Result<c_int>`
// and compute the ioctl request number like the C `_IOR`/`_IOWR` macros ('a' = CEC).
nix::ioctl_readwrite!(cec_adap_g_caps, b'a', 0, CecCaps);
nix::ioctl_read!(cec_adap_g_phys_addr, b'a', 1, u16);
nix::ioctl_read!(cec_adap_g_log_addrs, b'a', 3, CecLogAddrs);
nix::ioctl_readwrite!(cec_adap_s_log_addrs, b'a', 4, CecLogAddrs);
nix::ioctl_readwrite!(cec_transmit, b'a', 5, CecMsg);

impl CecCaps {
    fn empty() -> CecCaps {
        CecCaps { driver: [0; 32], name: [0; 32], available_log_addrs: 0, capabilities: 0, version: 0 }
    }
}

impl CecLogAddrs {
    fn empty() -> CecLogAddrs {
        CecLogAddrs {
            log_addr: [0; CEC_MAX_LOG_ADDRS],
            log_addr_mask: 0,
            cec_version: 0,
            num_log_addrs: 0,
            vendor_id: 0,
            flags: 0,
            osd_name: [0; 15],
            primary_device_type: [0; CEC_MAX_LOG_ADDRS],
            log_addr_type: [0; CEC_MAX_LOG_ADDRS],
            all_device_types: [0; CEC_MAX_LOG_ADDRS],
            features: [[0; 12]; CEC_MAX_LOG_ADDRS],
        }
    }

    fn claimed_addresses(&self) -> Vec<u8> {
        (0u8..BROADCAST_ADDRESS).filter(|address: &u8| self.log_addr_mask & (1 << address) != 0).collect()
    }
}

impl CecMsg {
    fn directed(initiator: u8, destination: u8, opcode: Option<u8>, reply: u8) -> CecMsg {
        let mut msg: [u8; CEC_MAX_MSG_SIZE] = [0; CEC_MAX_MSG_SIZE];
        msg[0] = (initiator << 4) | destination;
        let len: u32 = match opcode {
            Some(code) => {
                msg[1] = code;
                2
            }
            // A message with only the header byte is a "polling message":
            // an ACK means a device owns that logical address.
            None => 1,
        };
        CecMsg {
            tx_ts: 0,
            rx_ts: 0,
            len,
            timeout: if reply == 0 { 0 } else { REPLY_TIMEOUT_MS },
            sequence: 0,
            flags: 0,
            msg,
            reply,
            rx_status: 0,
            tx_status: 0,
            tx_arb_lost_cnt: 0,
            tx_nack_cnt: 0,
            tx_low_drive_cnt: 0,
            tx_error_cnt: 0,
        }
    }

    fn payload(&self) -> &[u8] {
        let end: usize = (self.len as usize).min(CEC_MAX_MSG_SIZE);
        if end <= 2 { &[] } else { &self.msg[2..end] }
    }
}

pub fn scan(device: &Path, claim_logical_address: bool, osd_name: &str) -> Result<CecReport, CecError> {
    // Ownership: `file` owns the descriptor and closes it when it goes out of
    // scope at the end of this function. `fd` is just a borrowed integer copy,
    // valid only while `file` is alive — which is why it is never stored.
    let file: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .map_err(|source: std::io::Error| CecError::Open { device: device.display().to_string(), source })?;
    let fd: RawFd = file.as_raw_fd();
    let mut warnings: Vec<String> = Vec::new();

    let caps: CecCaps = read_caps(fd)?;
    let physical_address: u16 = read_physical_address(fd)?;
    let mut log_addrs: CecLogAddrs = read_log_addrs(fd)?;
    let mut claimed_now: bool = false;

    if log_addrs.log_addr_mask == 0 {
        let can_claim: bool = caps.capabilities & CEC_CAP_LOG_ADDRS != 0;
        if physical_address == CEC_PHYS_ADDR_INVALID {
            warnings.push("no physical address: the TV is off, disconnected, or has not sent its EDID".to_string());
        } else if claim_logical_address && can_claim {
            claim_playback_address(fd, osd_name)?;
            log_addrs = read_log_addrs(fd)?;
            claimed_now = true;
        } else {
            warnings.push("the adapter has no logical address, devices cannot be queried".to_string());
        }
    }

    let own_addresses: Vec<u8> = log_addrs.claimed_addresses();
    let mut devices: Vec<CecDeviceInfo> = Vec::new();
    if let Some(initiator) = own_addresses.first().copied() {
        for address in 0u8..BROADCAST_ADDRESS {
            if own_addresses.contains(&address) {
                continue;
            }
            if poll(fd, initiator, address)? {
                devices.push(describe_device(fd, initiator, address));
            }
        }
    }

    let adapter: CecAdapterInfo = CecAdapterInfo {
        device: device.display().to_string(),
        driver: c_string(&caps.driver),
        name: c_string(&caps.name),
        physical_address: format_physical_address(physical_address),
        logical_addresses: own_addresses,
        claimed_now,
    };
    Ok(CecReport { adapter, devices, warnings })
}

fn read_caps(fd: RawFd) -> Result<CecCaps, CecError> {
    let mut caps: CecCaps = CecCaps::empty();
    // SAFETY: `caps` is a live, properly aligned `#[repr(C)]` struct with the exact
    // size of `struct cec_caps` (checked at compile time above); the kernel writes
    // at most that many bytes into it, and `fd` is an open CEC device.
    unsafe { cec_adap_g_caps(fd, &mut caps) }
        .map_err(|source: nix::errno::Errno| CecError::Ioctl { operation: "CEC_ADAP_G_CAPS", source })?;
    Ok(caps)
}

fn read_physical_address(fd: RawFd) -> Result<u16, CecError> {
    let mut address: u16 = CEC_PHYS_ADDR_INVALID;
    // SAFETY: the kernel writes one __u16 into `address`, a valid u16 on our stack.
    unsafe { cec_adap_g_phys_addr(fd, &mut address) }
        .map_err(|source: nix::errno::Errno| CecError::Ioctl { operation: "CEC_ADAP_G_PHYS_ADDR", source })?;
    Ok(address)
}

fn read_log_addrs(fd: RawFd) -> Result<CecLogAddrs, CecError> {
    let mut log_addrs: CecLogAddrs = CecLogAddrs::empty();
    // SAFETY: same reasoning as `read_caps`, for `struct cec_log_addrs`.
    unsafe { cec_adap_g_log_addrs(fd, &mut log_addrs) }
        .map_err(|source: nix::errno::Errno| CecError::Ioctl { operation: "CEC_ADAP_G_LOG_ADDRS", source })?;
    Ok(log_addrs)
}

/// Registers this Pi on the bus as a "Playback Device", the role media players use.
/// The kernel keeps the address after the descriptor is closed.
fn claim_playback_address(fd: RawFd, osd_name: &str) -> Result<(), CecError> {
    let mut request: CecLogAddrs = CecLogAddrs::empty();
    request.cec_version = CEC_OP_CEC_VERSION_1_4;
    request.num_log_addrs = 1;
    request.vendor_id = CEC_VENDOR_ID_NONE;
    // At most 14 bytes, so the 15-byte C array stays NUL-terminated.
    let name_bytes: &[u8] = osd_name.as_bytes();
    let name_length: usize = name_bytes.len().min(request.osd_name.len() - 1);
    request.osd_name[..name_length].copy_from_slice(&name_bytes[..name_length]);
    request.primary_device_type[0] = CEC_OP_PRIM_DEVTYPE_PLAYBACK;
    request.log_addr_type[0] = CEC_LOG_ADDR_TYPE_PLAYBACK;
    request.all_device_types[0] = CEC_OP_ALL_DEVTYPE_PLAYBACK;
    // SAFETY: `request` is a valid `struct cec_log_addrs`; the kernel reads it and
    // writes the claimed addresses back into the same memory. In blocking mode the
    // call returns once claiming is finished (well under a second with a valid
    // physical address, which the caller has checked).
    unsafe { cec_adap_s_log_addrs(fd, &mut request) }
        .map_err(|source: nix::errno::Errno| CecError::Ioctl { operation: "CEC_ADAP_S_LOG_ADDRS", source })?;
    Ok(())
}

fn send(fd: RawFd, message: &mut CecMsg) -> Result<(), CecError> {
    // SAFETY: `message` is a valid `struct cec_msg` that we exclusively borrow for
    // the duration of the call; the kernel fills in status fields and, when
    // `reply` is set, overwrites `msg`/`len` with the reply in place.
    unsafe { cec_transmit(fd, message) }
        .map_err(|source: nix::errno::Errno| CecError::Ioctl { operation: "CEC_TRANSMIT", source })?;
    Ok(())
}

fn poll(fd: RawFd, initiator: u8, destination: u8) -> Result<bool, CecError> {
    let mut message: CecMsg = CecMsg::directed(initiator, destination, None, 0);
    send(fd, &mut message)?;
    Ok(message.tx_status & CEC_TX_STATUS_OK != 0)
}

/// Sends `opcode` and waits for `reply_opcode`; `None` when the device did not
/// answer, refused (Feature Abort), or the transmission failed.
fn query(fd: RawFd, initiator: u8, destination: u8, opcode: u8, reply_opcode: u8) -> Option<Vec<u8>> {
    let mut message: CecMsg = CecMsg::directed(initiator, destination, Some(opcode), reply_opcode);
    send(fd, &mut message).ok()?;
    let answered: bool = message.tx_status & CEC_TX_STATUS_OK != 0 && message.rx_status & CEC_RX_STATUS_OK != 0;
    answered.then(|| message.payload().to_vec())
}

fn describe_device(fd: RawFd, initiator: u8, address: u8) -> CecDeviceInfo {
    let physical_address: Option<String> =
        query(fd, initiator, address, OPCODE_GIVE_PHYSICAL_ADDR, OPCODE_REPORT_PHYSICAL_ADDR)
            .filter(|payload: &Vec<u8>| payload.len() >= 2)
            .and_then(|payload: Vec<u8>| format_physical_address(u16::from_be_bytes([payload[0], payload[1]])));
    let osd_name: Option<String> = query(fd, initiator, address, OPCODE_GIVE_OSD_NAME, OPCODE_SET_OSD_NAME)
        .map(|payload: Vec<u8>| String::from_utf8_lossy(&payload).trim().to_string());
    let vendor: Option<u32> = query(fd, initiator, address, OPCODE_GIVE_DEVICE_VENDOR_ID, OPCODE_DEVICE_VENDOR_ID)
        .filter(|payload: &Vec<u8>| payload.len() >= 3)
        .map(|payload: Vec<u8>| u32::from_be_bytes([0, payload[0], payload[1], payload[2]]));
    let power_status: Option<CecPowerStatus> =
        query(fd, initiator, address, OPCODE_GIVE_DEVICE_POWER_STATUS, OPCODE_REPORT_POWER_STATUS)
            .and_then(|payload: Vec<u8>| payload.first().copied())
            .map(CecPowerStatus::from_operand);
    let cec_version: Option<&'static str> = query(fd, initiator, address, OPCODE_GET_CEC_VERSION, OPCODE_CEC_VERSION)
        .and_then(|payload: Vec<u8>| payload.first().copied())
        .and_then(cec_version_name);
    CecDeviceInfo {
        logical_address: address,
        role: role_of(address),
        physical_address,
        osd_name,
        vendor_id: vendor.map(|id: u32| format!("0x{id:06x}")),
        vendor_name: vendor.and_then(vendor_name),
        power_status,
        cec_version,
    }
}

fn c_string(bytes: &[u8]) -> String {
    let end: usize = bytes.iter().position(|byte: &u8| *byte == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_message_is_header_only() {
        let message: CecMsg = CecMsg::directed(4, 0, None, 0);
        assert_eq!(message.len, 1);
        assert_eq!(message.msg[0], 0x40);
        assert_eq!(message.timeout, 0);
    }

    #[test]
    fn query_message_carries_opcode_and_waits_for_reply() {
        let message: CecMsg = CecMsg::directed(4, 0, Some(OPCODE_GIVE_DEVICE_POWER_STATUS), OPCODE_REPORT_POWER_STATUS);
        assert_eq!(message.len, 2);
        assert_eq!(&message.msg[..2], &[0x40, 0x8F]);
        assert_eq!(message.reply, 0x90);
        assert_eq!(message.timeout, REPLY_TIMEOUT_MS);
    }

    #[test]
    fn payload_skips_header_and_opcode() {
        let mut message: CecMsg = CecMsg::directed(0, 4, Some(OPCODE_SET_OSD_NAME), 0);
        message.msg[2..4].copy_from_slice(b"TV");
        message.len = 4;
        assert_eq!(message.payload(), b"TV");
    }

    #[test]
    fn claimed_addresses_come_from_the_mask() {
        let mut log_addrs: CecLogAddrs = CecLogAddrs::empty();
        log_addrs.log_addr_mask = (1 << 4) | (1 << 8);
        assert_eq!(log_addrs.claimed_addresses(), vec![4, 8]);
    }
}
