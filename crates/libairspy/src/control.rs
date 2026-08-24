//! Device control-surface setters, ported from `airspy.c`.

use crate::commands::Command;
use crate::device::Device;
use crate::error::{Error, Result};
use crate::transfer::{NO_WINDEX, NO_WVALUE};

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
        let n = self.vendor_out(Command::SetFreq, NO_WVALUE, NO_WINDEX, &payload)?;
        if n < payload.len() {
            // C: result < length → AIRSPY_ERROR_LIBUSB.
            return Err(Error::TransferLengthMismatch {
                expected: payload.len(),
                actual: n,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::device::Device;
    use crate::transport::mock::{MockTransport, wire};

    /// A Device over a mock transport, with construction-time traffic
    /// (the samplerate query) already drained.
    pub(crate) fn mock_device() -> (Arc<MockTransport>, Device) {
        let transport = Arc::new(MockTransport::default());
        // Unscripted reads fail, so the open-time samplerate query
        // takes the C fallback table deterministically.
        let device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        transport.take_recorded();
        (transport, device)
    }

    #[test]
    fn freq_payload_is_little_endian() {
        // set_freq_params.freq_hz = TO_LE(freq_hz): 100 MHz on the
        // wire is 00 E1 F5 05.
        assert_eq!(100_000_000u32.to_le_bytes(), [0x00, 0xE1, 0xF5, 0x05]);
    }

    #[test]
    fn set_freq_wire_contract() {
        // The full airspy_set_freq transfer: bmRequestType 0x40,
        // bRequest AIRSPY_SET_FREQ (13), zero wValue/wIndex, 4-byte
        // little-endian payload, LIBUSB_CTRL_TIMEOUT_MS.
        let (transport, device) = mock_device();
        device.set_freq(100_000_000).expect("set_freq");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::SET_FREQ);
        assert_eq!((c.value, c.index), (0, 0));
        assert_eq!(c.data, vec![0x00, 0xE1, 0xF5, 0x05]);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn set_freq_short_write_maps_to_length_mismatch() {
        let (transport, device) = mock_device();
        transport.script_writes(vec![Ok(vec![0u8; 2])]); // 2 of 4 bytes
        let err = device.set_freq(1).expect_err("short write");
        assert!(matches!(
            err,
            crate::Error::TransferLengthMismatch {
                expected: 4,
                actual: 2
            }
        ));
    }

    #[test]
    fn set_freq_usb_error_passes_through() {
        let (transport, device) = mock_device();
        transport.script_writes(vec![Err(rusb::Error::Pipe)]);
        let err = device.set_freq(1).expect_err("usb error");
        assert!(matches!(err, crate::Error::Usb(rusb::Error::Pipe)));
    }

    #[test]
    fn board_id_read_wire_contract_and_value() {
        let (transport, device) = mock_device();
        // Non-zero id: proves response bytes actually reach the
        // caller (a zeroed buffer can't fake it).
        transport.script_reads(vec![Ok(vec![0x42u8])]);
        let id = device.board_id().expect("board id");
        assert_eq!(id, 0x42);
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_IN);
        assert_eq!(c.request, wire::BOARD_ID_READ);
        assert_eq!((c.value, c.index), (0, 0));
        assert_eq!(c.data.len(), 1);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn stop_rx_sends_receiver_off_even_without_stream() {
        // airspy_stop_rx sends RECEIVER_MODE_OFF unconditionally.
        let (transport, mut device) = mock_device();
        device.stop_rx().expect("stop");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::RECEIVER_MODE);
        assert_eq!(c.value, wire::RECEIVER_MODE_OFF);
        assert_eq!(c.index, 0);
        assert!(c.data.is_empty());
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn from_transport_caches_scripted_samplerates() {
        let transport = Arc::new(MockTransport::default());
        // Count query (4 bytes LE) then the rate list.
        transport.script_reads(vec![
            Ok(2u32.to_le_bytes().to_vec()),
            Ok([6_000_000u32, 3_000_000u32]
                .iter()
                .flat_map(|r| r.to_le_bytes())
                .collect()),
        ]);
        let device = Device::from_transport(transport as Arc<_>);
        // Default type is Float32Iq: rates undoubled.
        assert_eq!(device.samplerates(), vec![6_000_000, 3_000_000]);
    }

    #[test]
    fn from_transport_falls_back_when_query_fails() {
        let (_, device) = mock_device();
        // Fallback pair {10, 2.5} MSPS, undoubled for the IQ default.
        assert_eq!(device.samplerates(), vec![10_000_000, 2_500_000]);
    }
}
