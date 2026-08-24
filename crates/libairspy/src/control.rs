//! Device control-surface setters, ported from `airspy.c`.

use crate::commands::Command;
use crate::device::Device;
use crate::error::{Error, Result};

impl Device {
    /// Tune the receiver (`airspy_set_freq`): the target frequency in
    /// Hz travels as a 4-byte little-endian payload (`set_freq_params_t`
    /// with `TO_LE`, airspy.c).
    ///
    /// The C header documents 24 MHz – 1.75 GHz as the valid range but
    /// the library does not enforce it, and neither does this port —
    /// the firmware is the authority.
    pub fn set_freq(&self, freq_hz: u32) -> Result<()> {
        let payload = freq_hz.to_le_bytes();
        let n = self.vendor_out(Command::SetFreq, 0, 0, &payload)?;
        if n < payload.len() {
            // C: result < length → AIRSPY_ERROR_LIBUSB.
            return Err(Error::Usb(rusb::Error::Other));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn freq_payload_is_little_endian() {
        // set_freq_params.freq_hz = TO_LE(freq_hz): 100 MHz on the
        // wire is 00 E1 F5 05.
        assert_eq!(100_000_000u32.to_le_bytes(), [0x00, 0xE1, 0xF5, 0x05]);
    }
}
