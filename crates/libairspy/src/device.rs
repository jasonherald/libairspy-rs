//! Device enumeration and lifecycle, ported from `airspy.c`'s
//! `airspy_list_devices` / `airspy_open_device` / `airspy_open_exit`.
//!
//! `airspy_open`/`airspy_close` pairs become RAII: [`Device::open`]
//! claims the interface, and dropping the [`Device`] releases it and
//! closes the handle. The Android `airspy_open_fd` path is out of
//! scope (see the design spec).

use rusb::UsbContext as _;

use crate::error::{Error, Result};

/// USB vendor id (`airspy_usb_vid` in airspy.c).
pub const AIRSPY_USB_VID: u16 = 0x1d50;
/// USB product id (`airspy_usb_pid` in airspy.c).
pub const AIRSPY_USB_PID: u16 = 0x60a1;

/// `STR_PREFIX_SERIAL_AIRSPY_SIZE` — chars before the hex digits
/// ("AIRSPY SN:").
const SERIAL_PREFIX_LEN: usize = 10;
/// `SERIAL_AIRSPY_EXPECTED_SIZE` — total serial-descriptor length.
const SERIAL_EXPECTED_LEN: usize = 26;

/// Parse an Airspy serial-number string descriptor into its `u64`
/// serial, mirroring the C library: the descriptor must be exactly
/// [`SERIAL_EXPECTED_LEN`] chars, and the tail after the prefix is fed
/// through `strtoull(_, _, 16)` — parsing stops at the first non-hex
/// character and fails only when no digit was consumed at all.
fn parse_serial(descriptor: &str) -> Option<u64> {
    if descriptor.len() != SERIAL_EXPECTED_LEN {
        return None;
    }
    let hex = &descriptor[SERIAL_PREFIX_LEN..];
    let digits: &str = hex
        .find(|c: char| !c.is_ascii_hexdigit())
        .map_or(hex, |end| &hex[..end]);
    if digits.is_empty() {
        return None;
    }
    // At most 16 hex digits fit in the 26-char descriptor, so this
    // cannot overflow u64.
    u64::from_str_radix(digits, 16).ok()
}

/// Read and parse the serial descriptor of an open handle. `None`
/// mirrors every C skip-path: no descriptor index, read failure, wrong
/// length, or unparseable digits.
fn read_serial<T: rusb::UsbContext>(
    handle: &rusb::DeviceHandle<T>,
    descriptor: &rusb::DeviceDescriptor,
) -> Option<u64> {
    let serial = handle.read_serial_number_string_ascii(descriptor).ok()?;
    parse_serial(&serial)
}

/// List the serial numbers of all connected Airspy devices, mirroring
/// `airspy_list_devices` (devices whose serial cannot be read or
/// parsed are skipped, as in C).
pub fn list_devices() -> Result<Vec<u64>> {
    let context = rusb::Context::new()?;
    let devices = context.devices().map_err(|_| Error::NotFound)?;
    let mut serials = Vec::new();
    for dev in devices.iter() {
        let Ok(descriptor) = dev.device_descriptor() else {
            continue;
        };
        if descriptor.vendor_id() != AIRSPY_USB_VID || descriptor.product_id() != AIRSPY_USB_PID {
            continue;
        }
        let Ok(handle) = dev.open() else { continue };
        if let Some(serial) = read_serial(&handle, &descriptor) {
            serials.push(serial);
        }
    }
    Ok(serials)
}

/// An open Airspy device with interface 0 claimed.
///
/// Dropping the device releases the interface and closes the USB
/// handle (`airspy_open_exit` semantics).
#[derive(Debug)]
pub struct Device {
    handle: rusb::DeviceHandle<rusb::Context>,
}

impl Device {
    /// Open the first Airspy found (`airspy_open`).
    pub fn open() -> Result<Self> {
        Self::open_impl(None)
    }

    /// Open the Airspy with the given serial number (`airspy_open_sn`).
    pub fn open_serial(serial_number: u64) -> Result<Self> {
        Self::open_impl(Some(serial_number))
    }

    fn open_impl(serial_number: Option<u64>) -> Result<Self> {
        let context = rusb::Context::new()?;
        let devices = context.devices().map_err(|_| Error::NotFound)?;
        for dev in devices.iter() {
            let Ok(descriptor) = dev.device_descriptor() else {
                continue;
            };
            if descriptor.vendor_id() != AIRSPY_USB_VID || descriptor.product_id() != AIRSPY_USB_PID
            {
                continue;
            }
            let Ok(mut handle) = dev.open() else { continue };
            if let Some(wanted) = serial_number {
                // C additionally requires iSerialNumber > 0 and the
                // exact expected descriptor length; read_serial folds
                // those into its None path.
                match read_serial(&handle, &descriptor) {
                    Some(serial) if serial == wanted => {}
                    _ => continue,
                }
            }
            if Self::configure(&mut handle).is_err() {
                // C closes the handle and keeps scanning on any
                // configuration/claim failure.
                continue;
            }
            return Ok(Self { handle });
        }
        Err(Error::NotFound)
    }

    /// Kernel-driver detach + `set_configuration(1)` +
    /// `claim_interface(0)`, exactly as `airspy_open_device` does.
    fn configure(handle: &mut rusb::DeviceHandle<rusb::Context>) -> Result<()> {
        #[cfg(target_os = "linux")]
        if handle.kernel_driver_active(0).unwrap_or(false) {
            let _ = handle.detach_kernel_driver(0);
        }
        handle.set_active_configuration(1)?;
        handle.claim_interface(0)?;
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // airspy_open_exit: libusb_release_interface(usb_device, 0)
        // then close; rusb's own Drop handles the close/exit half.
        let _ = self.handle.release_interface(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_ids_match_c_constants() {
        // airspy_usb_vid / airspy_usb_pid in airspy.c.
        assert_eq!(AIRSPY_USB_VID, 0x1d50);
        assert_eq!(AIRSPY_USB_PID, 0x60a1);
    }

    #[test]
    fn parses_wellformed_serial_descriptor() {
        // SERIAL_AIRSPY_EXPECTED_SIZE (26) chars: a 10-char prefix
        // (STR_PREFIX_SERIAL_AIRSPY_SIZE) then 16 hex digits.
        assert_eq!(
            parse_serial("AIRSPY SN:0123456789ABCDEF"),
            Some(0x0123_4567_89AB_CDEF)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:00000000644064DC"),
            Some(0x6440_64DC)
        );
        // strtoull accepts lowercase hex too.
        assert_eq!(
            parse_serial("AIRSPY SN:00000000644064dc"),
            Some(0x6440_64DC)
        );
    }

    #[test]
    fn rejects_serial_with_wrong_length() {
        // C checks serial_number_len == SERIAL_AIRSPY_EXPECTED_SIZE
        // before parsing at all.
        assert_eq!(parse_serial("AIRSPY SN:123"), None);
        assert_eq!(parse_serial(""), None);
        assert_eq!(parse_serial("AIRSPY SN:0123456789ABCDEF0"), None);
    }

    #[test]
    fn rejects_serial_with_no_hex_digits() {
        // C rejects only when strtoull consumed nothing
        // (serial == 0 && start == end).
        assert_eq!(parse_serial("AIRSPY SN:ZZZZZZZZZZZZZZZZ"), None);
    }

    #[test]
    fn accepts_partial_hex_like_strtoull() {
        // Faithful strtoull semantics: parsing stops at the first
        // non-hex character, and the value parsed so far is accepted
        // as long as at least one digit was consumed.
        assert_eq!(parse_serial("AIRSPY SN:12ZZZZZZZZZZZZZZ"), Some(0x12));
    }

    #[test]
    fn zero_serial_parses_as_zero() {
        // All-zero digits: start != end, so C accepts serial 0.
        assert_eq!(parse_serial("AIRSPY SN:0000000000000000"), Some(0));
    }

    // The tests below exercise the real USB stack but require no
    // hardware: with no Airspy plugged in, enumeration finds nothing
    // and open reports NotFound exactly as the C library does.

    #[test]
    fn list_devices_without_hardware_is_empty() {
        let serials = list_devices().expect("USB enumeration should succeed");
        assert!(
            serials.is_empty(),
            "expected no Airspy devices in the test environment"
        );
    }

    #[test]
    fn open_without_hardware_reports_not_found() {
        assert!(matches!(Device::open(), Err(crate::Error::NotFound)));
        assert!(matches!(
            Device::open_serial(0x1234_5678_9ABC_DEF0),
            Err(crate::Error::NotFound)
        ));
    }
}
