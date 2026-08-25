//! Device control-surface setters, ported from `airspy.c`.

use crate::commands::{Command, GpioPin, GpioPort, SampleType};
use crate::device::Device;
use crate::error::{Error, Result};
use crate::stream::BULK_ENDPOINT;
use crate::transfer::{NO_WINDEX, NO_WVALUE};

/// `MIN_SAMPLERATE_BY_VALUE` (airspy.c) — arguments at or above this
/// are literal rates in Hz; below it they are table indices.
const MIN_SAMPLERATE_BY_VALUE: u32 = 1_000_000;

/// `GAIN_COUNT` (airspy.c) — entries in each composite-gain table.
const GAIN_COUNT: usize = 22;

// The composite-gain tables from airspy.c (BSD-3-Clause), transcribed
// verbatim: airspy_linearity_*_gains / airspy_sensitivity_*_gains.
#[rustfmt::skip]
const LINEARITY_VGA_GAINS: [u8; GAIN_COUNT] =
    [13, 12, 11, 11, 11, 11, 11, 10, 10, 10, 10, 10, 10, 10, 10, 10, 9, 8, 7, 6, 5, 4];
#[rustfmt::skip]
const LINEARITY_MIXER_GAINS: [u8; GAIN_COUNT] =
    [12, 12, 11, 9, 8, 7, 6, 6, 5, 0, 0, 1, 0, 0, 2, 2, 1, 1, 1, 1, 0, 0];
#[rustfmt::skip]
const LINEARITY_LNA_GAINS: [u8; GAIN_COUNT] =
    [14, 14, 14, 13, 12, 10, 9, 9, 8, 9, 8, 6, 5, 3, 1, 0, 0, 0, 0, 0, 0, 0];
#[rustfmt::skip]
const SENSITIVITY_VGA_GAINS: [u8; GAIN_COUNT] =
    [13, 12, 11, 10, 9, 8, 7, 6, 5, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 4];
#[rustfmt::skip]
const SENSITIVITY_MIXER_GAINS: [u8; GAIN_COUNT] =
    [12, 12, 12, 12, 11, 10, 10, 9, 9, 8, 7, 4, 4, 4, 3, 2, 2, 1, 0, 0, 0, 0];
#[rustfmt::skip]
const SENSITIVITY_LNA_GAINS: [u8; GAIN_COUNT] =
    [14, 14, 14, 14, 14, 14, 14, 14, 14, 13, 12, 12, 9, 9, 8, 7, 6, 5, 3, 2, 1, 0];

/// `airspy_set_lna_gain`'s clamp bound.
const LNA_GAIN_MAX: u8 = 14;
/// `airspy_set_mixer_gain` / `airspy_set_vga_gain`'s clamp bound.
const MIXER_VGA_GAIN_MAX: u8 = 15;

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

impl Device {
    /// The IN-with-status pattern most C setters share: a 1-byte read
    /// with the value in `wIndex`, `result < 1` mapping to the libusb
    /// error.
    fn in_setter(&self, command: Command, index: u16) -> Result<()> {
        let mut status = [0u8; 1];
        let n = self.vendor_in(command, NO_WVALUE, index, &mut status)?;
        if n < status.len() {
            return Err(Error::TransferLengthMismatch {
                expected: status.len(),
                actual: n,
            });
        }
        Ok(())
    }

    /// Select the sample rate (`airspy_set_samplerate`): the argument
    /// is either an index into [`Device::samplerates`]'s table or a
    /// literal rate in Hz (at or above `MIN_SAMPLERATE_BY_VALUE`).
    /// Off-table literal rates are sent in kHz, doubled first for IQ
    /// sample types, exactly as C computes them.
    pub fn set_samplerate(&self, samplerate: u32) -> Result<()> {
        let mut value = samplerate;
        if value >= MIN_SAMPLERATE_BY_VALUE {
            if let Some(i) = self.raw_samplerates().iter().position(|&r| r == value) {
                value = u32::try_from(i).unwrap_or(u32::MAX);
            } else {
                if matches!(
                    self.sample_type(),
                    SampleType::Float32Iq | SampleType::Int16Iq
                ) {
                    value = value.wrapping_mul(2);
                }
                value /= 1000;
            }
        }
        // C clears the bulk endpoint halt before the request and
        // ignores the result.
        let _ = self.usb_handle().clear_halt(BULK_ENDPOINT);
        // C passes the u32 into libusb's u16 wIndex — same truncation.
        #[allow(clippy::cast_possible_truncation)]
        self.in_setter(Command::SetSamplerate, value as u16)
    }

    /// `airspy_set_lna_gain` (0–14; larger values clamp, like C).
    pub fn set_lna_gain(&self, value: u8) -> Result<()> {
        self.in_setter(Command::SetLnaGain, u16::from(value.min(LNA_GAIN_MAX)))
    }

    /// `airspy_set_mixer_gain` (0–15; larger values clamp, like C).
    pub fn set_mixer_gain(&self, value: u8) -> Result<()> {
        self.in_setter(
            Command::SetMixerGain,
            u16::from(value.min(MIXER_VGA_GAIN_MAX)),
        )
    }

    /// `airspy_set_vga_gain` (0–15; larger values clamp, like C).
    pub fn set_vga_gain(&self, value: u8) -> Result<()> {
        self.in_setter(
            Command::SetVgaGain,
            u16::from(value.min(MIXER_VGA_GAIN_MAX)),
        )
    }

    /// `airspy_set_lna_agc`.
    pub fn set_lna_agc(&self, enabled: bool) -> Result<()> {
        self.in_setter(Command::SetLnaAgc, u16::from(enabled))
    }

    /// `airspy_set_mixer_agc`.
    pub fn set_mixer_agc(&self, enabled: bool) -> Result<()> {
        self.in_setter(Command::SetMixerAgc, u16::from(enabled))
    }

    /// The shared composite-gain sequence of `airspy_set_linearity_gain`
    /// and `airspy_set_sensitivity_gain`: clamp, reverse the index,
    /// disable both AGCs, then write the three table gains.
    fn set_composite_gain(
        &self,
        value: u8,
        vga: &[u8; GAIN_COUNT],
        mixer: &[u8; GAIN_COUNT],
        lna: &[u8; GAIN_COUNT],
    ) -> Result<()> {
        let clamped = usize::from(value).min(GAIN_COUNT - 1);
        let idx = GAIN_COUNT - 1 - clamped;
        self.set_mixer_agc(false)?;
        self.set_lna_agc(false)?;
        self.set_vga_gain(vga[idx])?;
        self.set_mixer_gain(mixer[idx])?;
        self.set_lna_gain(lna[idx])
    }

    /// `airspy_set_linearity_gain` (0–21).
    pub fn set_linearity_gain(&self, value: u8) -> Result<()> {
        self.set_composite_gain(
            value,
            &LINEARITY_VGA_GAINS,
            &LINEARITY_MIXER_GAINS,
            &LINEARITY_LNA_GAINS,
        )
    }

    /// `airspy_set_sensitivity_gain` (0–21).
    pub fn set_sensitivity_gain(&self, value: u8) -> Result<()> {
        self.set_composite_gain(
            value,
            &SENSITIVITY_VGA_GAINS,
            &SENSITIVITY_MIXER_GAINS,
            &SENSITIVITY_LNA_GAINS,
        )
    }

    /// `airspy_gpio_write`: `wValue` carries the level, `wIndex`
    /// packs `port << 5 | pin`. Crate-internal until the full GPIO
    /// surface lands.
    pub(crate) fn gpio_write(&self, port: GpioPort, pin: GpioPin, value: u8) -> Result<()> {
        let port_pin = u16::from((port as u8) << 5 | pin as u8);
        let payload: [u8; 0] = [];
        let n = self.vendor_out(Command::GpioWrite, u16::from(value), port_pin, &payload)?;
        if n != payload.len() {
            return Err(Error::TransferLengthMismatch {
                expected: payload.len(),
                actual: n,
            });
        }
        Ok(())
    }

    /// Toggle the bias tee (`airspy_set_rf_bias` — a GPIO write to
    /// port 1 pin 13).
    pub fn set_rf_bias(&self, enabled: bool) -> Result<()> {
        self.gpio_write(GpioPort::Port1, GpioPin::Pin13, u8::from(enabled))
    }

    /// Toggle 12-bit packed transfers (`airspy_set_packing`). Refused
    /// with [`Error::Busy`] while streaming, like C.
    ///
    /// Deviation: C cancels and reallocates its transfer buffers here;
    /// this engine sizes its buffers per `start_rx`, so only the flag
    /// changes.
    pub fn set_packing(&mut self, enabled: bool) -> Result<()> {
        if self.is_streaming() {
            return Err(Error::Busy);
        }
        self.in_setter(Command::SetPacking, u16::from(enabled))?;
        self.packing_enabled = enabled;
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
        // Construct directly so the construction-time recording is
        // retained: the fallback must be reached BECAUSE the firmware
        // query failed, not silently.
        let transport = Arc::new(MockTransport::default());
        let device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1, "exactly the count query");
        assert_eq!(calls[0].request, wire::GET_SAMPLERATES);
        // The C fallback pair, undoubled for the IQ default; asserted
        // against the independent wire transcription so production
        // drift is caught.
        assert_eq!(device.samplerates(), wire::FALLBACK_SAMPLERATES.to_vec());
    }

    /// The IN-with-status pattern most setters use: one `read_control`
    /// with the value in wIndex and a 1-byte status buffer.
    fn assert_in_setter(c: &crate::transport::mock::ControlCall, request: u8, index: u16) {
        assert_eq!(c.request_type, wire::VENDOR_IN);
        assert_eq!(c.request, request);
        assert_eq!((c.value, c.index), (0, index));
        assert_eq!(c.data.len(), 1);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn set_samplerate_by_index_matches_table() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8])]);
        // 2_500_000 is index 1 of the fallback table.
        device.set_samplerate(2_500_000).expect("set");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[0], wire::SET_SAMPLERATE, 1);
    }

    #[test]
    fn set_samplerate_by_value_scales_for_iq() {
        let (transport, mut device) = mock_device();
        // Default type is Float32Iq: an off-table rate doubles then
        // divides by 1000 (6 MHz -> 12000).
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_samplerate(6_000_000).expect("set");
        assert_in_setter(&transport.take_recorded()[0], wire::SET_SAMPLERATE, 12_000);

        // A real (non-IQ) type skips the doubling (6 MHz -> 6000).
        device
            .set_sample_type(crate::commands::SampleType::Int16Real)
            .expect("type");
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_samplerate(6_000_000).expect("set");
        assert_in_setter(&transport.take_recorded()[0], wire::SET_SAMPLERATE, 6_000);
    }

    #[test]
    fn set_samplerate_small_values_are_raw_indices() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_samplerate(0).expect("set");
        assert_in_setter(&transport.take_recorded()[0], wire::SET_SAMPLERATE, 0);
    }

    #[test]
    fn gain_setters_clamp_like_c() {
        let (transport, device) = mock_device();
        // LNA clamps to 14; mixer and VGA clamp to 15.
        transport.script_reads(vec![Ok(vec![0u8]); 3]);
        device.set_lna_gain(200).expect("lna");
        device.set_mixer_gain(200).expect("mixer");
        device.set_vga_gain(200).expect("vga");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[0], wire::SET_LNA_GAIN, 14);
        assert_in_setter(&calls[1], wire::SET_MIXER_GAIN, 15);
        assert_in_setter(&calls[2], wire::SET_VGA_GAIN, 15);
    }

    #[test]
    fn agc_setters_send_bool_as_byte() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8]); 2]);
        device.set_lna_agc(true).expect("lna agc");
        device.set_mixer_agc(false).expect("mixer agc");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[0], wire::SET_LNA_AGC, 1);
        assert_in_setter(&calls[1], wire::SET_MIXER_AGC, 0);
    }

    #[test]
    fn linearity_gain_runs_c_sequence_with_table_values() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8]); 5]);
        // value 0 -> reversed index 21: vga 4, mixer 0, lna 0.
        device.set_linearity_gain(0).expect("linearity");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 5);
        assert_in_setter(&calls[0], wire::SET_MIXER_AGC, 0);
        assert_in_setter(&calls[1], wire::SET_LNA_AGC, 0);
        assert_in_setter(&calls[2], wire::SET_VGA_GAIN, 4);
        assert_in_setter(&calls[3], wire::SET_MIXER_GAIN, 0);
        assert_in_setter(&calls[4], wire::SET_LNA_GAIN, 0);
    }

    #[test]
    fn sensitivity_gain_clamps_and_uses_its_tables() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8]); 5]);
        // Any value >= 22 clamps to 21 -> reversed index 0:
        // vga 13, mixer 12, lna 14.
        device.set_sensitivity_gain(200).expect("sensitivity");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[2], wire::SET_VGA_GAIN, 13);
        assert_in_setter(&calls[3], wire::SET_MIXER_GAIN, 12);
        assert_in_setter(&calls[4], wire::SET_LNA_GAIN, 14);
    }

    #[test]
    fn rf_bias_writes_gpio_port1_pin13() {
        let (transport, device) = mock_device();
        device.set_rf_bias(true).expect("bias");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::GPIO_WRITE);
        assert_eq!(c.value, 1);
        // port 1 << 5 | pin 13 = 45.
        assert_eq!(c.index, 45);
        assert!(c.data.is_empty());
    }

    #[test]
    fn set_packing_updates_flag_and_wire() {
        let (transport, mut device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_packing(true).expect("packing");
        assert!(device.packing_enabled);
        assert_in_setter(&transport.take_recorded()[0], wire::SET_PACKING, 1);
    }
}
