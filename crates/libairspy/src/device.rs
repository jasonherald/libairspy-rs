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
/// `SERIAL_NUMBER_UNUSED` — `airspy_open_sn` treats serial 0 as "no
/// filter" and opens the first device found.
const SERIAL_NUMBER_UNUSED: u64 = 0;

/// USB configuration selected on open (`libusb_set_configuration(dev_handle, 1)`
/// in `airspy_open_device`).
const USB_CONFIGURATION: u8 = 1;
/// USB interface claimed on open and released on close
/// (`libusb_claim_interface(dev_handle, 0)` in `airspy_open_device`,
/// `libusb_release_interface(usb_device, 0)` in `airspy_open_exit`).
const USB_INTERFACE: u8 = 0;

/// Map an `airspy_open_sn`-style serial argument to an optional filter:
/// `SERIAL_NUMBER_UNUSED` (0) means "open the first device".
const fn serial_filter(serial_number: u64) -> Option<u64> {
    if serial_number == SERIAL_NUMBER_UNUSED {
        None
    } else {
        Some(serial_number)
    }
}

/// Parse an Airspy serial-number string descriptor into its `u64`
/// serial, mirroring the C library: the descriptor must be exactly
/// [`SERIAL_EXPECTED_LEN`] chars, and the tail after the prefix is fed
/// through `strtoull(_, _, 16)` — parsing stops at the first non-hex
/// character and fails only when no digit was consumed at all.
fn parse_serial(descriptor: &str) -> Option<u64> {
    if descriptor.len() != SERIAL_EXPECTED_LEN {
        return None;
    }
    // C skips SERIAL_PREFIX_LEN raw bytes without validating their
    // content; `get` keeps that semantic while refusing (rather than
    // panicking) if byte 10 is not a character boundary.
    let tail = descriptor.get(SERIAL_PREFIX_LEN..)?;
    strtoull_16(tail)
}

/// The `strtoull(start, &end, 16)` contract the C serial parse relies
/// on: skip leading whitespace, accept one optional sign ('-' negates
/// with unsigned wraparound), accept an optional `0x`/`0X` prefix when
/// a hex digit follows it, then consume hex digits greedily. `None`
/// iff no digit was consumed (C's `start == end` reject path).
fn strtoull_16(s: &str) -> Option<u64> {
    // C isspace(): space, \t, \n, \v, \f, \r — one char wider than
    // Rust's is_ascii_whitespace(), which lacks vertical tab.
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0B', '\x0C', '\r']);
    let (negative, s) = match s.strip_prefix(['+', '-']) {
        Some(rest) => (s.starts_with('-'), rest),
        None => (false, s),
    };
    // The prefix is consumed only when a hex digit follows; bare "0x"
    // parses as the digit '0' with 'x' left unconsumed.
    let s = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_hexdigit()) => rest,
        _ => s,
    };
    let digits: &str = s
        .find(|c: char| !c.is_ascii_hexdigit())
        .map_or(s, |end| &s[..end]);
    if digits.is_empty() {
        return None;
    }
    // At most 16 hex digits fit in the 26-char descriptor, so the
    // parse cannot overflow u64 (strtoull would saturate; unreachable
    // at this input length).
    let value = u64::from_str_radix(digits, 16).ok()?;
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
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

/// Enumerate connected devices matching the Airspy VID/PID, yielding
/// each with its descriptor. Centralizes the filter both
/// `airspy_list_devices` and `airspy_open_device` perform in C.
fn airspy_devices(
    context: &rusb::Context,
) -> Result<Vec<(rusb::Device<rusb::Context>, rusb::DeviceDescriptor)>> {
    let devices = context.devices().map_err(|_| Error::NotFound)?;
    Ok(devices
        .iter()
        .filter_map(|dev| {
            let descriptor = dev.device_descriptor().ok()?;
            (descriptor.vendor_id() == AIRSPY_USB_VID && descriptor.product_id() == AIRSPY_USB_PID)
                .then_some((dev, descriptor))
        })
        .collect())
}

/// List the serial numbers of all connected Airspy devices, mirroring
/// `airspy_list_devices` (devices whose serial cannot be read or
/// parsed are skipped, as in C).
pub fn list_devices() -> Result<Vec<u64>> {
    let context = rusb::Context::new()?;
    let mut serials = Vec::new();
    for (dev, descriptor) in airspy_devices(&context)? {
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
    ///
    /// Serial `0` is `SERIAL_NUMBER_UNUSED` in the C library: it opens
    /// the first device found instead of matching a literal zero
    /// serial, and this port preserves that contract.
    pub fn open_serial(serial_number: u64) -> Result<Self> {
        Self::open_impl(serial_filter(serial_number))
    }

    fn open_impl(serial_number: Option<u64>) -> Result<Self> {
        let context = rusb::Context::new()?;
        for (dev, descriptor) in airspy_devices(&context)? {
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
        if handle.kernel_driver_active(USB_INTERFACE).unwrap_or(false) {
            let _ = handle.detach_kernel_driver(USB_INTERFACE);
        }
        handle.set_active_configuration(USB_CONFIGURATION)?;
        handle.claim_interface(USB_INTERFACE)?;
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // airspy_open_exit: libusb_release_interface(usb_device, 0)
        // then close; rusb's own Drop handles the close/exit half.
        let _ = self.handle.release_interface(USB_INTERFACE);
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
        // Whitespace alone consumes no digits either.
        assert_eq!(parse_serial("AIRSPY SN:                "), None);
        // A sign with no digits after it consumes nothing.
        assert_eq!(parse_serial("AIRSPY SN:-ZZZZZZZZZZZZZZZ"), None);
    }

    #[test]
    fn skips_leading_whitespace_like_strtoull() {
        assert_eq!(
            parse_serial("AIRSPY SN:  123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:\t 123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        // C isspace() includes vertical tab, which Rust's
        // is_ascii_whitespace() does not.
        assert_eq!(
            parse_serial("AIRSPY SN:\x0B 123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
    }

    #[test]
    fn accepts_sign_like_strtoull() {
        assert_eq!(
            parse_serial("AIRSPY SN:+123456789ABCDEF"),
            Some(0x123_4567_89AB_CDEF)
        );
        // strtoull negates on '-' with unsigned wraparound.
        assert_eq!(
            parse_serial("AIRSPY SN:-1ZZZZZZZZZZZZZZ"),
            Some(1u64.wrapping_neg())
        );
    }

    #[test]
    fn accepts_hex_prefix_like_strtoull() {
        assert_eq!(
            parse_serial("AIRSPY SN:0x123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:0X123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        // "0x" not followed by a hex digit: strtoull consumes just the
        // "0" and stops, yielding 0 with digits consumed (accepted).
        assert_eq!(parse_serial("AIRSPY SN:0xZZZZZZZZZZZZZZ"), Some(0));
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

    #[test]
    fn does_not_panic_on_multibyte_descriptor() {
        // The 10-byte skip must not panic when byte 10 falls inside a
        // multibyte character; C operates on raw bytes, we return None.
        let s = "AIRSPY SNé0123456789ABCDE"; // 'é' spans bytes 9–10; 26 bytes total
        assert_eq!(s.len(), SERIAL_EXPECTED_LEN);
        assert_eq!(parse_serial(s), None);
    }

    #[test]
    fn zero_serial_is_the_unused_sentinel() {
        // SERIAL_NUMBER_UNUSED (0ULL) in airspy.c: airspy_open_sn(0)
        // opens the first device instead of matching serial 0.
        assert_eq!(serial_filter(0), None);
        assert_eq!(serial_filter(0x6440_64DC), Some(0x6440_64DC));
    }

    // The tests below exercise the real USB stack and are written to
    // hold in any environment — CI runners with no USB devices AND a
    // dev box with a real Airspy attached.

    #[test]
    fn list_devices_enumerates_without_error() {
        // Zero devices in CI; each attached Airspy contributes a
        // parsed serial. Every u64 is a valid serial (C appends
        // whatever strtoull parsed), so success is the only invariant.
        let _serials = list_devices().expect("USB enumeration should succeed");
    }

    #[test]
    fn open_returns_device_or_not_found() {
        // Success and NotFound are both valid depending on what is
        // attached; open and enumeration are deliberately not equated —
        // open() reads no serial when unfiltered, and a listed device
        // can still fail configuration (both mirror C behavior). Any
        // other error means the scan loop broke.
        match Device::open() {
            Ok(_) | Err(crate::Error::NotFound) => {}
            Err(other) => unreachable!("unexpected error: {other}"),
        }
    }

    #[test]
    fn open_with_unknown_serial_reports_not_found() {
        // Pick a nonzero serial provably absent from the attached
        // device set, so the test holds even with hardware plugged in.
        let serials = list_devices().expect("USB enumeration should succeed");
        let unknown_serial = (1..=u64::MAX)
            .find(|serial| !serials.contains(serial))
            .expect("a finite device list cannot contain every nonzero u64");
        assert!(matches!(
            Device::open_serial(unknown_serial),
            Err(crate::Error::NotFound)
        ));
    }
}
