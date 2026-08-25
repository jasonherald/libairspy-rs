//! Shared logic for the `airspy_rx` binary, ported from
//! `airspy-tools/src/airspy_rx.c`: WAV header handling, C-semantics
//! argument parsing helpers, transfer byte accounting, and the
//! streaming-rate tracker. Everything here is hardware-free and
//! unit-tested; the binary wires it to a live device.

use libairspy_rs::Samples;
use libairspy_rs::commands::SampleType;

use crate::{ParseU64Error, parse_u64};

/// `FREQ_ONE_MHZ` in `airspy_rx.c`.
pub const FREQ_ONE_MHZ: u32 = 1_000_000;
/// `DEFAULT_FREQ_HZ` (900 MHz) in `airspy_rx.c`.
pub const DEFAULT_FREQ_HZ: u32 = 900_000_000;
/// `FREQ_HZ_MIN` (24 MHz) in `airspy_rx.c`.
pub const FREQ_HZ_MIN: u32 = 24_000_000;
/// `FREQ_HZ_MAX` in `airspy_rx.c` — "1900MHz (officially 1750MHz)".
pub const FREQ_HZ_MAX: u32 = 1_900_000_000;
/// `DEFAULT_VGA_IF_GAIN` in `airspy_rx.c`.
pub const DEFAULT_VGA_IF_GAIN: u32 = 5;
/// `DEFAULT_LNA_GAIN` in `airspy_rx.c`.
pub const DEFAULT_LNA_GAIN: u32 = 1;
/// `DEFAULT_MIXER_GAIN` in `airspy_rx.c`.
pub const DEFAULT_MIXER_GAIN: u32 = 5;
/// `VGA_GAIN_MAX` in `airspy_rx.c`.
pub const VGA_GAIN_MAX: u32 = 15;
/// `MIXER_GAIN_MAX` in `airspy_rx.c`.
pub const MIXER_GAIN_MAX: u32 = 15;
/// `LNA_GAIN_MAX` in `airspy_rx.c`.
pub const LNA_GAIN_MAX: u32 = 14;
/// `LINEARITY_GAIN_MAX` in `airspy_rx.c`.
pub const LINEARITY_GAIN_MAX: u32 = 21;
/// `SENSITIVITY_GAIN_MAX` in `airspy_rx.c`.
pub const SENSITIVITY_GAIN_MAX: u32 = 21;
/// `BIAST_MAX` in `airspy_rx.c`.
pub const BIAST_MAX: u32 = 1;
/// `SAMPLES_TO_XFER_MAX_U64` in `airspy_rx.c`.
pub const SAMPLES_TO_XFER_MAX: u64 = 0x8000_0000_0000_0000;
/// `MIN_SAMPLERATE_BY_VALUE` in `airspy_rx.c` — `-a` values at or below
/// this are sample-rate-table indices, larger values are literal Hz.
pub const MIN_SAMPLERATE_BY_VALUE: u32 = 1_000_000;
/// `FD_BUFFER_SIZE` in `airspy_rx.c` — the `setvbuf` output buffer.
pub const FD_BUFFER_SIZE: usize = 16 * 1024;

/// `sizeof(t_wav_file_hdr)` — 12-byte RIFF header + 24-byte format
/// chunk + 8-byte data-chunk header, no padding.
pub const WAV_HEADER_LEN: usize = 44;

/// The most data bytes a classic RIFF/WAV file can carry: the 32-bit
/// size fields cap the whole file at `u32::MAX` bytes, header
/// included. C wraps its `uint32_t` `ftell` position past 4 GiB and
/// writes corrupt size fields; deviation: `-w` captures stop at this
/// budget so the finalized header is always representable.
pub const WAV_MAX_DATA_BYTES: u64 = u32::MAX as u64 - WAV_HEADER_LEN as u64;

/// The per-buffer window of the C rate average (`buffer_count == 50`
/// in `rx_callback`).
const RATE_WINDOW_BUFFERS: u32 = 50;
/// The EWMA weight in `rx_callback`'s
/// `average_rate += 0.2f * (rate - average_rate)`.
const RATE_EWMA_WEIGHT: f32 = 0.2;

/// The WAV format-chunk fields selected by `-t` in `airspy_rx.c` `main()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavParams {
    /// `wFormatTag`: 1 = PCM, 3 = IEEE float.
    pub format_tag: u16,
    /// `wChannels`: 2 for IQ, 1 for real/raw.
    pub channels: u16,
    /// `wBitsPerSample`.
    pub bits_per_sample: u16,
}

impl WavParams {
    /// The `-t` switch mapping in `airspy_rx.c` `main()`. The RAW case
    /// sets only bits (12) and channels (1) in C; the format tag
    /// keeps the PCM default (RAW is rejected for WAV output anyway).
    pub fn for_sample_type(sample_type: SampleType) -> Self {
        match sample_type {
            SampleType::Float32Iq => Self {
                format_tag: 3,
                channels: 2,
                bits_per_sample: 32,
            },
            SampleType::Float32Real => Self {
                format_tag: 3,
                channels: 1,
                bits_per_sample: 32,
            },
            SampleType::Int16Iq => Self {
                format_tag: 1,
                channels: 2,
                bits_per_sample: 16,
            },
            SampleType::Int16Real | SampleType::Uint16Real => Self {
                format_tag: 1,
                channels: 1,
                bits_per_sample: 16,
            },
            SampleType::Raw => Self {
                format_tag: 1,
                channels: 1,
                bits_per_sample: 12,
            },
        }
    }

    /// `wav_nb_byte_per_sample` (`wBitsPerSample / 8`).
    pub fn bytes_per_sample(&self) -> u16 {
        self.bits_per_sample / 8
    }

    /// `wBlockAlign` (`wChannels * wBitsPerSample / 8`).
    pub fn block_align(&self) -> u16 {
        self.channels * (self.bits_per_sample / 8)
    }
}

/// The initial 44-byte header written before streaming — C's static
/// `wave_file_hdr` initializer: chunk IDs and the fixed fmt-chunk size
/// present, every "to update later" field zero.
pub fn wav_header_placeholder() -> [u8; WAV_HEADER_LEN] {
    let mut bytes = [0u8; WAV_HEADER_LEN];
    bytes[0..4].copy_from_slice(b"RIFF");
    bytes[8..12].copy_from_slice(b"WAVE");
    bytes[12..16].copy_from_slice(b"fmt ");
    bytes[16..20].copy_from_slice(&16u32.to_le_bytes());
    bytes[36..40].copy_from_slice(b"data");
    bytes
}

/// The end-of-capture header rewrite in `airspy_rx.c` `main()`:
/// `size = file_pos - 8`, `data.chunkSize = file_pos - sizeof(hdr)`,
/// format fields from the `-t` selection.
///
/// Deviation: C writes `dwAvgBytesPerSec = dwSamplesPerSec *
/// wav_nb_byte_per_sample`, dropping the channel count — half the
/// true byte rate for 2-channel IQ captures. The WAV spec value is
/// `dwSamplesPerSec * wBlockAlign`, written here.
pub fn wav_header_finalized(
    file_pos: u32,
    params: &WavParams,
    samples_per_sec: u32,
) -> [u8; WAV_HEADER_LEN] {
    let mut bytes = wav_header_placeholder();
    bytes[4..8].copy_from_slice(&(file_pos.wrapping_sub(8)).to_le_bytes());
    bytes[20..22].copy_from_slice(&params.format_tag.to_le_bytes());
    bytes[22..24].copy_from_slice(&params.channels.to_le_bytes());
    bytes[24..28].copy_from_slice(&samples_per_sec.to_le_bytes());
    let avg_bytes_per_sec = samples_per_sec.wrapping_mul(u32::from(params.block_align()));
    bytes[28..32].copy_from_slice(&avg_bytes_per_sec.to_le_bytes());
    bytes[32..34].copy_from_slice(&params.block_align().to_le_bytes());
    bytes[34..36].copy_from_slice(&params.bits_per_sample.to_le_bytes());
    // WAV_HEADER_LEN is 44; the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    let data_len = file_pos.wrapping_sub(WAV_HEADER_LEN as u32);
    bytes[40..44].copy_from_slice(&data_len.to_le_bytes());
    bytes
}

/// `parse_u32` in `airspy_rx.c`: the same prefix detection as
/// [`parse_u64`], then `strtoul` — which is 64-bit on the LP64
/// platforms upstream targets, so out-of-range values saturate to
/// `ULONG_MAX` and then truncate into the `uint32_t` result.
#[allow(clippy::cast_possible_truncation)]
pub fn parse_u32(s: &str) -> Result<u32, ParseU64Error> {
    parse_u64(s).map(|v| v as u32)
}

/// `strtod(s, NULL)`'s longest-valid-prefix parse, restricted to the
/// decimal forms the tool sees: optional whitespace and sign, digits
/// with an optional fraction, optional exponent. C's hex-float and
/// inf/nan spellings are not recognized (they parse as 0, the
/// no-conversion result).
fn strtod_prefix(s: &str) -> f64 {
    let trimmed = s.trim_start_matches([' ', '\t', '\n', '\x0B', '\x0C', '\r']);
    let bytes = trimmed.as_bytes();
    let mut pos = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        pos += 1;
    }
    let int_digits = bytes[pos..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    pos += int_digits;
    let mut frac_digits = 0;
    if bytes.get(pos) == Some(&b'.') {
        frac_digits = bytes[pos + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        pos += 1 + frac_digits;
    }
    if int_digits == 0 && frac_digits == 0 {
        return 0.0;
    }
    if matches!(bytes.get(pos), Some(b'e' | b'E')) {
        let mut exp_pos = pos + 1;
        if matches!(bytes.get(exp_pos), Some(b'+' | b'-')) {
            exp_pos += 1;
        }
        let exp_digits = bytes[exp_pos..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        // The exponent is only part of the number when at least one
        // digit follows, matching strtod's longest-match rule.
        if exp_digits > 0 {
            pos = exp_pos + exp_digits;
        }
    }
    trimmed[..pos].parse().unwrap_or(0.0)
}

/// The `-f` branch in `airspy_rx.c` `main()`: `strtod(optarg, NULL) *
/// FREQ_ONE_MHZ`, with results above `FREQ_HZ_MAX` becoming
/// `UINT_MAX` so the later range check rejects them. (Rust's
/// saturating float→int cast replaces C's out-of-range cast UB; both
/// land outside the accepted range.)
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn parse_freq_mhz(s: &str) -> u32 {
    let freq_hz = strtod_prefix(s) * f64::from(FREQ_ONE_MHZ);
    if freq_hz <= f64::from(FREQ_HZ_MAX) {
        freq_hz as u32
    } else {
        u32::MAX
    }
}

/// `bytes_to_xfer = samples_to_xfer * wav_nb_bits_per_sample *
/// wav_nb_channels / 8` in `airspy_rx.c` `main()`.
///
/// Deviation: C uses the RAW 12-bit figure whether or not packing is
/// enabled, so unpacked RAW captures (16-bit words on the wire) stop
/// at 75% of the requested samples; the unpacked case uses 16 bits
/// here.
pub fn bytes_to_xfer(
    samples_to_xfer: u64,
    params: &WavParams,
    sample_type: SampleType,
    packing: bool,
) -> u64 {
    let bits = if sample_type == SampleType::Raw && !packing {
        16
    } else {
        u64::from(params.bits_per_sample)
    };
    // C's unsigned arithmetic wraps; the only values that can wrap
    // are >= SAMPLES_TO_XFER_MAX, which the caller rejects anyway.
    samples_to_xfer
        .wrapping_mul(bits)
        .wrapping_mul(u64::from(params.channels))
        / 8
}

/// The sample-rate resolution in `airspy_rx.c` `main()`: values at or
/// below [`MIN_SAMPLERATE_BY_VALUE`] index the supported-rate table
/// (`None` when out of range — C errors out), larger values are
/// literal Hz.
pub fn resolve_display_rate(sample_rate_val: u32, rates: &[u32]) -> Option<u32> {
    if sample_rate_val <= MIN_SAMPLERATE_BY_VALUE {
        rates.get(sample_rate_val as usize).copied()
    } else {
        Some(sample_rate_val)
    }
}

/// The `-w` automatic filename: `AirSpy_%sZ_%ukHz_IQ.wav` with the
/// `%Y%m%d_%H%M%S` local time and `freq_hz / 1000` (`airspy_rx.c`
/// `main()`).
pub fn wav_filename(date_time: &str, freq_hz: u32) -> String {
    format!("AirSpy_{date_time}Z_{}kHz_IQ.wav", freq_hz / 1000)
}

/// The streaming-rate bookkeeping from `rx_callback` in `airspy_rx.c`:
/// the first packet latches the start times; afterwards every
/// 50th buffer (`RATE_WINDOW_BUFFERS`) computes the window rate, applies
/// the 0.2 EWMA into `average_rate`, and accumulates
/// `global_average_rate` / `rate_samples` for the exit summary.
#[derive(Debug)]
pub struct RateTracker {
    /// C's `average_rate`, seeded with the nominal sample rate.
    pub average_rate: f32,
    /// C's `global_average_rate` (sum of window averages).
    pub global_average_rate: f32,
    /// C's `rate_samples` (number of completed windows).
    pub rate_samples: u32,
    /// C's `t_start` — the first packet's arrival, for the total-time
    /// summary.
    pub t_start: Option<f64>,
    time_start: f64,
    buffer_count: u32,
    sample_count: u32,
}

impl RateTracker {
    /// `average_rate = (float)wav_sample_per_sec` before the C main
    /// loop.
    pub fn new(nominal_rate: f32) -> Self {
        Self {
            average_rate: nominal_rate,
            global_average_rate: 0.0,
            rate_samples: 0,
            t_start: None,
            time_start: 0.0,
            buffer_count: 0,
            sample_count: 0,
        }
    }

    /// One delivered buffer of `samples` samples at time `now`
    /// (seconds; any epoch — only differences are used).
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn on_block(&mut self, samples: u32, now: f64) {
        if self.t_start.is_none() {
            self.t_start = Some(now);
            self.time_start = now;
            return;
        }
        self.buffer_count += 1;
        self.sample_count += samples;
        if self.buffer_count == RATE_WINDOW_BUFFERS {
            // TimevalDiff returns float; the window rate is float math
            // in C too.
            let time_difference = (now - self.time_start) as f32;
            let rate = self.sample_count as f32 / time_difference;
            self.average_rate += RATE_EWMA_WEIGHT * (rate - self.average_rate);
            self.global_average_rate += self.average_rate;
            self.rate_samples += 1;
            self.time_start = now;
            self.sample_count = 0;
            self.buffer_count = 0;
        }
    }
}

/// The `-n` limiting from `rx_callback` in `airspy_rx.c`: clamp the
/// block to the remaining budget and deduct what will be written.
/// Returns the byte count to write; `None` means unlimited.
#[allow(clippy::cast_possible_truncation)]
pub fn apply_byte_budget(remaining: &mut Option<u64>, block_len: usize) -> usize {
    let Some(remaining) = remaining.as_mut() else {
        return block_len;
    };
    // C: if (bytes_to_write >= bytes_to_xfer) bytes_to_write =
    // bytes_to_xfer; bytes_to_xfer -= bytes_to_write;
    let bytes_to_write = if block_len as u64 >= *remaining {
        *remaining as usize
    } else {
        block_len
    };
    *remaining -= bytes_to_write as u64;
    bytes_to_write
}

/// C's `transfer->sample_count` for a delivered block: frames (IQ
/// pairs count once) for the converted types, and for RAW the sample
/// count behind the wire bytes — `len * 2 / 3` packed 12-bit,
/// `len / 2` unpacked 16-bit words.
#[allow(clippy::cast_possible_truncation)]
pub fn frame_count(samples: &Samples<'_>, sample_type: SampleType, packing: bool) -> u32 {
    let frames = match samples {
        Samples::Float32(s) if sample_type == SampleType::Float32Iq => s.len() / 2,
        Samples::Int16(s) if sample_type == SampleType::Int16Iq => s.len() / 2,
        Samples::Float32(s) => s.len(),
        Samples::Int16(s) => s.len(),
        Samples::Uint16(s) => s.len(),
        Samples::Raw(s) => {
            if packing {
                s.len() * 2 / 3
            } else {
                s.len() / 2
            }
        }
    };
    frames as u32
}

/// Append a delivered block's bytes as `rx_callback`'s `fwrite` does —
/// the sample memory verbatim, which is little-endian on the wire and
/// in the WAV/raw file formats.
pub fn extend_sample_bytes(out: &mut Vec<u8>, samples: &Samples<'_>) {
    // One up-front reservation instead of a capacity check per
    // element — the caller reuses the buffer, so this is free after
    // the first block.
    match samples {
        Samples::Float32(s) => {
            out.reserve(std::mem::size_of_val(*s));
            for v in *s {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Samples::Int16(s) => {
            out.reserve(std::mem::size_of_val(*s));
            for v in *s {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Samples::Uint16(s) => {
            out.reserve(std::mem::size_of_val(*s));
            for v in *s {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Samples::Raw(s) => out.extend_from_slice(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libairspy_rs::Samples;
    use libairspy_rs::commands::SampleType;

    #[test]
    fn constants_match_c_defines() {
        // airspy_rx.c #defines.
        assert_eq!(FREQ_ONE_MHZ, 1_000_000);
        assert_eq!(DEFAULT_FREQ_HZ, 900_000_000);
        assert_eq!(FREQ_HZ_MIN, 24_000_000);
        assert_eq!(FREQ_HZ_MAX, 1_900_000_000);
        assert_eq!(DEFAULT_VGA_IF_GAIN, 5);
        assert_eq!(DEFAULT_LNA_GAIN, 1);
        assert_eq!(DEFAULT_MIXER_GAIN, 5);
        assert_eq!(VGA_GAIN_MAX, 15);
        assert_eq!(MIXER_GAIN_MAX, 15);
        assert_eq!(LNA_GAIN_MAX, 14);
        assert_eq!(LINEARITY_GAIN_MAX, 21);
        assert_eq!(SENSITIVITY_GAIN_MAX, 21);
        assert_eq!(BIAST_MAX, 1);
        assert_eq!(SAMPLES_TO_XFER_MAX, 0x8000_0000_0000_0000);
        assert_eq!(MIN_SAMPLERATE_BY_VALUE, 1_000_000);
        assert_eq!(FD_BUFFER_SIZE, 16 * 1024);
    }

    #[test]
    fn wav_params_map_sample_types_like_c() {
        // The -t switch cases in airspy_rx.c main(): format tag 3 is
        // IEEE float, 1 is PCM.
        let float_iq = WavParams::for_sample_type(SampleType::Float32Iq);
        assert_eq!(
            (
                float_iq.format_tag,
                float_iq.channels,
                float_iq.bits_per_sample
            ),
            (3, 2, 32)
        );
        let float_real = WavParams::for_sample_type(SampleType::Float32Real);
        assert_eq!(
            (
                float_real.format_tag,
                float_real.channels,
                float_real.bits_per_sample
            ),
            (3, 1, 32)
        );
        let int16_iq = WavParams::for_sample_type(SampleType::Int16Iq);
        assert_eq!(
            (
                int16_iq.format_tag,
                int16_iq.channels,
                int16_iq.bits_per_sample
            ),
            (1, 2, 16)
        );
        let int16_real = WavParams::for_sample_type(SampleType::Int16Real);
        assert_eq!((int16_real.channels, int16_real.bits_per_sample), (1, 16));
        let uint16_real = WavParams::for_sample_type(SampleType::Uint16Real);
        assert_eq!((uint16_real.channels, uint16_real.bits_per_sample), (1, 16));
        // RAW: 12 bits, mono (the case 5 branch sets only these two).
        let raw = WavParams::for_sample_type(SampleType::Raw);
        assert_eq!((raw.channels, raw.bits_per_sample), (1, 12));
    }

    #[test]
    fn wav_data_budget_keeps_sizes_representable() {
        // A maximal capture finalizes to file_pos = u32::MAX with
        // in-range RIFF and data sizes.
        assert_eq!(
            WAV_MAX_DATA_BYTES + WAV_HEADER_LEN as u64,
            u64::from(u32::MAX)
        );
        let params = WavParams::for_sample_type(SampleType::Int16Iq);
        #[allow(clippy::cast_possible_truncation)]
        let bytes = wav_header_finalized(u32::MAX, &params, 2_500_000);
        assert_eq!(bytes[4..8], (u32::MAX - 8).to_le_bytes());
        assert_eq!(bytes[40..44], (u32::MAX - 44).to_le_bytes());
    }

    #[test]
    fn wav_header_placeholder_matches_c_static_initializer() {
        // wave_file_hdr's static initializer: chunk IDs and the fixed
        // fmt-chunk size are set, every "to update later" field is 0.
        let bytes = wav_header_placeholder();
        assert_eq!(bytes.len(), WAV_HEADER_LEN);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(bytes[4..8], [0; 4]);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(bytes[16..20], 16u32.to_le_bytes());
        assert_eq!(bytes[20..36], [0; 16]);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes[40..44], [0; 4]);
    }

    #[test]
    fn wav_header_finalized_encodes_c_field_updates() {
        // The end-of-main header rewrite in airspy_rx.c: size =
        // file_pos - 8, data chunkSize = file_pos - sizeof(header),
        // fmt fields from the -t selection.
        let params = WavParams::for_sample_type(SampleType::Int16Iq);
        let bytes = wav_header_finalized(10_044, &params, 2_500_000);
        assert_eq!(bytes[4..8], 10_036u32.to_le_bytes());
        assert_eq!(bytes[20..22], 1u16.to_le_bytes()); // wFormatTag
        assert_eq!(bytes[22..24], 2u16.to_le_bytes()); // wChannels
        assert_eq!(bytes[24..28], 2_500_000u32.to_le_bytes());
        // dwAvgBytesPerSec — deviation: C writes dwSamplesPerSec *
        // wav_nb_byte_per_sample (5_000_000 here), dropping the
        // channel count; the WAV spec value is rate * block align.
        assert_eq!(bytes[28..32], 10_000_000u32.to_le_bytes());
        assert_eq!(bytes[32..34], 4u16.to_le_bytes()); // wBlockAlign
        assert_eq!(bytes[34..36], 16u16.to_le_bytes()); // wBitsPerSample
        assert_eq!(bytes[40..44], 10_000u32.to_le_bytes());
    }

    #[test]
    fn parse_u32_follows_c_strtoul_truncation() {
        // parse_u32 in airspy_rx.c stores strtoul's unsigned long
        // (64-bit on the LP64 platforms upstream targets) into a
        // uint32_t — values beyond 32 bits truncate, overflow
        // saturates to ULONG_MAX first.
        assert_eq!(parse_u32("42"), Ok(42));
        assert_eq!(parse_u32("0x10"), Ok(16));
        assert_eq!(parse_u32("0b101"), Ok(5));
        assert_eq!(parse_u32("0x1FFFFFFFF"), Ok(0xFFFF_FFFF));
        assert_eq!(parse_u32("-1"), Ok(0xFFFF_FFFF));
        assert_eq!(parse_u32("99999999999999999999999"), Ok(0xFFFF_FFFF));
        assert!(parse_u32("12abc").is_err());
        assert!(parse_u32("").is_err());
    }

    #[test]
    fn parse_freq_mhz_uses_strtod_prefix_semantics() {
        // The -f branch: strtod(optarg, NULL) * 1e6, then values above
        // FREQ_HZ_MAX become UINT_MAX (rejected by the later range
        // check); strtod parses the longest valid prefix and yields 0
        // when nothing parses.
        assert_eq!(parse_freq_mhz("100"), 100_000_000);
        assert_eq!(parse_freq_mhz("0.1"), 100_000);
        assert_eq!(parse_freq_mhz("12.5e1"), 125_000_000);
        assert_eq!(parse_freq_mhz(" 100"), 100_000_000);
        assert_eq!(parse_freq_mhz("100abc"), 100_000_000);
        assert_eq!(parse_freq_mhz("1900"), 1_900_000_000);
        assert_eq!(parse_freq_mhz("2000"), u32::MAX);
        assert_eq!(parse_freq_mhz("abc"), 0);
        assert_eq!(parse_freq_mhz(""), 0);
        // A negative parse cannot reach the valid range either way.
        assert!(parse_freq_mhz("-100") < FREQ_HZ_MIN);
    }

    #[test]
    fn bytes_to_xfer_matches_c_formula_with_raw_fix() {
        // C: bytes_to_xfer = samples * bits * channels / 8. For RAW
        // that always uses 12 bits — correct when packing is on,
        // wrong (unpacked words are 16-bit) when off; deviation: the
        // unpacked case uses 16 bits.
        let iq16 = WavParams::for_sample_type(SampleType::Int16Iq);
        assert_eq!(bytes_to_xfer(1000, &iq16, SampleType::Int16Iq, false), 4000);
        let fiq = WavParams::for_sample_type(SampleType::Float32Iq);
        assert_eq!(
            bytes_to_xfer(1000, &fiq, SampleType::Float32Iq, false),
            8000
        );
        let raw = WavParams::for_sample_type(SampleType::Raw);
        assert_eq!(bytes_to_xfer(1000, &raw, SampleType::Raw, true), 1500);
        assert_eq!(bytes_to_xfer(1000, &raw, SampleType::Raw, false), 2000);
        // Values at/above SAMPLES_TO_XFER_MAX wrap like C's unsigned
        // arithmetic; the caller rejects them right after computing.
        assert_eq!(
            bytes_to_xfer(SAMPLES_TO_XFER_MAX, &iq16, SampleType::Int16Iq, false),
            0
        );
    }

    #[test]
    fn resolve_display_rate_treats_small_values_as_indices() {
        // main() in airspy_rx.c: sample_rate_val <=
        // MIN_SAMPLERATE_BY_VALUE selects supported_samplerates[val]
        // (val >= count is an error); larger values are literal Hz.
        let rates = [10_000_000, 2_500_000];
        assert_eq!(resolve_display_rate(0, &rates), Some(10_000_000));
        assert_eq!(resolve_display_rate(1, &rates), Some(2_500_000));
        assert_eq!(resolve_display_rate(2, &rates), None);
        assert_eq!(resolve_display_rate(1_000_000, &rates), None);
        assert_eq!(resolve_display_rate(1_000_001, &rates), Some(1_000_001));
        assert_eq!(resolve_display_rate(6_000_000, &rates), Some(6_000_000));
    }

    #[test]
    fn wav_filename_matches_c_snprintf_format() {
        // snprintf(path_file, ..., "AirSpy_%sZ_%ukHz_IQ.wav",
        // date_time, freq_hz / 1000).
        assert_eq!(
            wav_filename("20261225_101112", 100_000_000),
            "AirSpy_20261225_101112Z_100000kHz_IQ.wav"
        );
        assert_eq!(
            wav_filename("20130101_000000", 900_000_000),
            "AirSpy_20130101_000000Z_900000kHz_IQ.wav"
        );
    }

    #[test]
    fn rate_tracker_mirrors_c_averaging() {
        // rx_callback in airspy_rx.c: the first packet only latches
        // the start times; afterwards every 50th buffer computes
        // rate = sample_count / elapsed, applies the 0.2 EWMA, and
        // accumulates the global average.
        let mut tracker = RateTracker::new(0.0);
        tracker.on_block(1000, 0.0);
        assert_eq!(tracker.rate_samples, 0);
        for i in 1..=49 {
            tracker.on_block(1000, f64::from(i) * 0.02);
        }
        assert_eq!(tracker.rate_samples, 0);
        tracker.on_block(1000, 1.0);
        // 50 blocks * 1000 samples over 1 s → rate 50_000; EWMA from
        // 0.0 → 0.2 * 50_000 = 10_000.
        assert_eq!(tracker.rate_samples, 1);
        assert!((tracker.average_rate - 10_000.0).abs() < 1.0);
        assert!((tracker.global_average_rate - 10_000.0).abs() < 1.0);
    }

    #[test]
    fn apply_byte_budget_clamps_and_deducts_like_c() {
        // No -n: everything is written, nothing tracked.
        let mut unlimited = None;
        assert_eq!(apply_byte_budget(&mut unlimited, 4096), 4096);
        assert_eq!(unlimited, None);
        // Budget larger than the block: full write, deducted.
        let mut remaining = Some(10_000u64);
        assert_eq!(apply_byte_budget(&mut remaining, 4096), 4096);
        assert_eq!(remaining, Some(5904));
        // Budget smaller than the block: clamp, then exhausted.
        let mut remaining = Some(100u64);
        assert_eq!(apply_byte_budget(&mut remaining, 4096), 100);
        assert_eq!(remaining, Some(0));
        // Exhausted budget writes nothing.
        assert_eq!(apply_byte_budget(&mut remaining, 4096), 0);
        assert_eq!(remaining, Some(0));
    }

    #[test]
    fn frame_count_matches_c_sample_count() {
        // IQ types count pairs; real types count elements; RAW counts
        // the samples behind the bytes (12-bit packed or 16-bit words).
        let f = [0.0f32; 8];
        assert_eq!(
            frame_count(&Samples::Float32(&f), SampleType::Float32Iq, false),
            4
        );
        assert_eq!(
            frame_count(&Samples::Float32(&f), SampleType::Float32Real, false),
            8
        );
        let i = [0i16; 8];
        assert_eq!(
            frame_count(&Samples::Int16(&i), SampleType::Int16Iq, false),
            4
        );
        assert_eq!(
            frame_count(&Samples::Int16(&i), SampleType::Int16Real, false),
            8
        );
        let u = [0u16; 8];
        assert_eq!(
            frame_count(&Samples::Uint16(&u), SampleType::Uint16Real, false),
            8
        );
        let raw = [0u8; 12];
        assert_eq!(frame_count(&Samples::Raw(&raw), SampleType::Raw, true), 8);
        assert_eq!(frame_count(&Samples::Raw(&raw), SampleType::Raw, false), 6);
    }

    #[test]
    fn extend_sample_bytes_serializes_little_endian() {
        // rx_callback writes the sample buffer's raw memory; the wire
        // and WAV formats are little-endian.
        let mut out = Vec::new();
        extend_sample_bytes(&mut out, &Samples::Int16(&[1, -2]));
        assert_eq!(out, [1, 0, 0xFE, 0xFF]);
        out.clear();
        extend_sample_bytes(&mut out, &Samples::Uint16(&[0x1234]));
        assert_eq!(out, [0x34, 0x12]);
        out.clear();
        extend_sample_bytes(&mut out, &Samples::Float32(&[1.0]));
        assert_eq!(out, 1.0f32.to_le_bytes());
        out.clear();
        extend_sample_bytes(&mut out, &Samples::Raw(&[9, 8, 7]));
        assert_eq!(out, [9, 8, 7]);
    }
}
