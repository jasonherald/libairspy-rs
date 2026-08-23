//! Device enumeration and lifecycle, ported from `airspy.c`'s
//! `airspy_list_devices` / `airspy_open_device` / `airspy_open_exit`.
//!
//! `airspy_open`/`airspy_close` pairs become RAII: [`Device::open`]
//! claims the interface, and dropping the [`Device`] releases it and
//! closes the handle. The Android `airspy_open_fd` path is out of
//! scope (see the design spec).

use rusb::UsbContext as _;

use crate::commands::Command;
use crate::error::{Error, Result};

/// USB vendor id (`airspy_usb_vid` in airspy.c).
pub const AIRSPY_USB_VID: u16 = 0x1d50;
/// USB product id (`airspy_usb_pid` in airspy.c).
pub const AIRSPY_USB_PID: u16 = 0x60a1;

/// `STR_PREFIX_SERIAL_AIRSPY_SIZE` in airspy.c — chars before the hex
/// digits ("AIRSPY SN:").
const SERIAL_PREFIX_LEN: usize = 10;
/// `SERIAL_AIRSPY_EXPECTED_SIZE` in airspy.c — total serial-descriptor
/// length.
const SERIAL_EXPECTED_LEN: usize = 26;
/// `SERIAL_NUMBER_UNUSED` in airspy.c — `airspy_open_sn` treats serial
/// 0 as "no filter" and opens the first device found.
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

/// Fallback rate table from `airspy_open_init` (airspy.c), used when
/// the firmware `AIRSPY_GET_SAMPLERATES` query fails.
const FALLBACK_SAMPLERATES: [u32; 2] = [10_000_000, 2_500_000];

/// Cap on the firmware-reported rate count: a control transfer's
/// wLength is a `u16`, so more than `u16::MAX / 4` words cannot
/// arrive in one read (C truncates the length cast instead).
const MAX_SAMPLERATE_COUNT: u32 = (u16::MAX as u32) / 4;

/// Decode little-endian `u32` sample rates from the bytes actually
/// transferred, dropping any partial trailing word (C leaves the tail
/// of its buffer uninitialized on a short read; we simply omit it).
fn samplerates_from_le_bytes(raw: &[u8]) -> Vec<u32> {
    raw.chunks_exact(4)
        .filter_map(|chunk| chunk.try_into().ok().map(u32::from_le_bytes))
        .collect()
}

/// Parse an Airspy serial-number string descriptor into its `u64`
/// serial, mirroring the C library: the descriptor must be exactly
/// [`SERIAL_EXPECTED_LEN`] chars, and the tail after the prefix is fed
/// through `strtoull(_, _, 16)` — parsing stops at the first non-hex
/// character and fails only when no digit was consumed at all.
fn parse_serial(descriptor: &str) -> Option<u64> {
    if descriptor.len() < SERIAL_EXPECTED_LEN {
        return None;
    }
    // C reads through libusb_get_string_descriptor_ascii into a
    // 27-byte buffer, which writes at most 26 chars — an over-long
    // descriptor arrives truncated to exactly SERIAL_EXPECTED_LEN and
    // is accepted. rusb returns the full string, so truncate to match.
    // `get` refuses (rather than panicking) if a cut lands off a
    // character boundary; the descriptor is ASCII in practice.
    let descriptor = descriptor.get(..SERIAL_EXPECTED_LEN)?;
    // C skips SERIAL_PREFIX_LEN raw bytes without validating their
    // content.
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
    supported_samplerates: Vec<u32>,
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
            let mut device = Self {
                handle,
                supported_samplerates: Vec::new(),
            };
            // airspy_open_init caches the firmware rate table at open,
            // falling back to a fixed pair when the query fails.
            device.supported_samplerates = device
                .read_samplerates_from_fw()
                .unwrap_or_else(|_| FALLBACK_SAMPLERATES.to_vec());
            return Ok(device);
        }
        Err(Error::NotFound)
    }

    /// The supported sample rates cached at open
    /// (`airspy_get_samplerates`; the C length/count two-call protocol
    /// collapses into a slice).
    ///
    /// C doubles these values when a real (non-IQ) sample type is
    /// selected; sample-type selection arrives with the streaming
    /// engine, which revisits this.
    #[must_use]
    pub fn samplerates(&self) -> &[u32] {
        &self.supported_samplerates
    }

    /// `airspy_read_samplerates_from_fw`: a 4-byte count query
    /// (wIndex = 0) followed by a count-word read (wIndex = count).
    fn read_samplerates_from_fw(&self) -> Result<Vec<u32>> {
        let mut count_raw = [0u8; 4];
        let n = self.vendor_in(Command::GetSamplerates, 0, 0, &mut count_raw)?;
        if n < 1 {
            // C: result < 1 → AIRSPY_ERROR_OTHER.
            return Err(Error::Other);
        }
        let count = u32::from_le_bytes(count_raw).min(MAX_SAMPLERATE_COUNT);
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut raw = vec![0u8; count as usize * 4];
        let index = u16::try_from(count).unwrap_or(u16::MAX);
        let n = self.vendor_in(Command::GetSamplerates, 0, index, &mut raw)?;
        if n < 1 {
            return Err(Error::Other);
        }
        Ok(samplerates_from_le_bytes(&raw[..n]))
    }

    /// Borrow the underlying USB handle; the vendor-request layer in
    /// `transfer.rs` builds on this.
    pub(crate) fn usb_handle(&self) -> &rusb::DeviceHandle<rusb::Context> {
        &self.handle
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
    fn rejects_serial_shorter_than_expected() {
        // C checks serial_number_len == SERIAL_AIRSPY_EXPECTED_SIZE
        // after a read that cannot exceed it, so short reads reject.
        assert_eq!(parse_serial("AIRSPY SN:123"), None);
        assert_eq!(parse_serial(""), None);
    }

    #[test]
    fn truncates_overlong_serial_like_c_buffer() {
        // libusb_get_string_descriptor_ascii writes at most 26 chars
        // into C's buffer, so a longer descriptor is truncated to 26
        // and accepted; rusb returns the whole string and we must
        // truncate to match.
        assert_eq!(
            parse_serial("AIRSPY SN:0123456789ABCDEF0"),
            Some(0x0123_4567_89AB_CDEF)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:0123456789ABCDEF-EXTRA-JUNK"),
            Some(0x0123_4567_89AB_CDEF)
        );
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

#[cfg(test)]
mod samplerate_tests {
    use super::*;

    #[test]
    fn fallback_table_matches_c() {
        // airspy_open_init's fallback when the firmware rate query
        // fails: {10000000, 2500000}.
        assert_eq!(FALLBACK_SAMPLERATES, [10_000_000, 2_500_000]);
    }

    #[test]
    fn samplerate_words_parse_little_endian() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&10_000_000u32.to_le_bytes());
        raw.extend_from_slice(&2_500_000u32.to_le_bytes());
        assert_eq!(samplerates_from_le_bytes(&raw), vec![10_000_000, 2_500_000]);
    }

    #[test]
    fn partial_trailing_word_is_dropped() {
        // Only complete words actually transferred are surfaced; C
        // would leave the tail of its malloc'd buffer uninitialized.
        let mut raw = Vec::new();
        raw.extend_from_slice(&6_000_000u32.to_le_bytes());
        raw.extend_from_slice(&[0x01, 0x02]);
        assert_eq!(samplerates_from_le_bytes(&raw), vec![6_000_000]);
    }
}
