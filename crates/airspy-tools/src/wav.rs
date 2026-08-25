//! WAV output support for `airspy_rx`, ported from the
//! `t_wav_file_hdr` structures, static initializer, and end-of-main
//! header rewrite in `airspy-tools/src/airspy_rx.c`.

use libairspy_rs::commands::SampleType;

/// `sizeof(t_wav_file_hdr)` — 12-byte RIFF header + 24-byte format
/// chunk + 8-byte data-chunk header, no padding.
pub const WAV_HEADER_LEN: usize = 44;

/// `wFormatTag` for integer PCM — the "1=PCM8/16" comment on
/// `t_FormatChunk` in `airspy_rx.c`.
pub const FORMAT_TAG_PCM: u16 = 1;
/// `wFormatTag` for IEEE float — the "3=Float32" comment on
/// `t_FormatChunk` in `airspy_rx.c`.
pub const FORMAT_TAG_IEEE_FLOAT: u16 = 3;

/// The fixed `chunkSize` of the format chunk ("16 fixed" in
/// `t_FormatChunk`, set in the `wave_file_hdr` initializer).
const FMT_CHUNK_SIZE: u32 = 16;
/// The RIFF `size` field excludes the 8-byte chunk id + size pair —
/// C's `file_pos - 8` in the header rewrite.
const RIFF_SIZE_EXCLUDED: u32 = 8;

// Byte offsets of the packed t_wav_file_hdr fields (t_WAVRIFF_hdr,
// then t_FormatChunk, then t_DataChunk — airspy_rx.c):
/// `t_WAVRIFF_hdr.size`.
const RIFF_SIZE_OFFSET: usize = 4;
/// `t_WAVRIFF_hdr.riffType`.
const RIFF_TYPE_OFFSET: usize = 8;
/// `t_FormatChunk.chunkID`.
const FMT_ID_OFFSET: usize = 12;
/// `t_FormatChunk.chunkSize`.
const FMT_SIZE_OFFSET: usize = 16;
/// `t_FormatChunk.wFormatTag`.
const FORMAT_TAG_OFFSET: usize = 20;
/// `t_FormatChunk.wChannels`.
const CHANNELS_OFFSET: usize = 22;
/// `t_FormatChunk.dwSamplesPerSec`.
const SAMPLES_PER_SEC_OFFSET: usize = 24;
/// `t_FormatChunk.dwAvgBytesPerSec`.
const AVG_BYTES_PER_SEC_OFFSET: usize = 28;
/// `t_FormatChunk.wBlockAlign`.
const BLOCK_ALIGN_OFFSET: usize = 32;
/// `t_FormatChunk.wBitsPerSample`.
const BITS_PER_SAMPLE_OFFSET: usize = 34;
/// `t_DataChunk.chunkID`.
const DATA_ID_OFFSET: usize = 36;
/// `t_DataChunk.chunkSize`.
const DATA_SIZE_OFFSET: usize = 40;

/// The most data bytes a classic RIFF/WAV file can carry: the 32-bit
/// size fields cap the whole file at `u32::MAX` bytes, header
/// included. C wraps its `uint32_t` `ftell` position past 4 GiB and
/// writes corrupt size fields; deviation: `-w` captures stop at this
/// budget so the finalized header is always representable.
pub const WAV_MAX_DATA_BYTES: u64 = u32::MAX as u64 - WAV_HEADER_LEN as u64;

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
                format_tag: FORMAT_TAG_IEEE_FLOAT,
                channels: 2,
                bits_per_sample: 32,
            },
            SampleType::Float32Real => Self {
                format_tag: FORMAT_TAG_IEEE_FLOAT,
                channels: 1,
                bits_per_sample: 32,
            },
            SampleType::Int16Iq => Self {
                format_tag: FORMAT_TAG_PCM,
                channels: 2,
                bits_per_sample: 16,
            },
            SampleType::Int16Real | SampleType::Uint16Real => Self {
                format_tag: FORMAT_TAG_PCM,
                channels: 1,
                bits_per_sample: 16,
            },
            SampleType::Raw => Self {
                format_tag: FORMAT_TAG_PCM,
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
    bytes[0..RIFF_SIZE_OFFSET].copy_from_slice(b"RIFF");
    bytes[RIFF_TYPE_OFFSET..FMT_ID_OFFSET].copy_from_slice(b"WAVE");
    bytes[FMT_ID_OFFSET..FMT_SIZE_OFFSET].copy_from_slice(b"fmt ");
    bytes[FMT_SIZE_OFFSET..FORMAT_TAG_OFFSET].copy_from_slice(&FMT_CHUNK_SIZE.to_le_bytes());
    bytes[DATA_ID_OFFSET..DATA_SIZE_OFFSET].copy_from_slice(b"data");
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
    let riff_size = file_pos.wrapping_sub(RIFF_SIZE_EXCLUDED);
    bytes[RIFF_SIZE_OFFSET..RIFF_TYPE_OFFSET].copy_from_slice(&riff_size.to_le_bytes());
    bytes[FORMAT_TAG_OFFSET..CHANNELS_OFFSET].copy_from_slice(&params.format_tag.to_le_bytes());
    bytes[CHANNELS_OFFSET..SAMPLES_PER_SEC_OFFSET].copy_from_slice(&params.channels.to_le_bytes());
    bytes[SAMPLES_PER_SEC_OFFSET..AVG_BYTES_PER_SEC_OFFSET]
        .copy_from_slice(&samples_per_sec.to_le_bytes());
    let avg_bytes_per_sec = samples_per_sec.wrapping_mul(u32::from(params.block_align()));
    bytes[AVG_BYTES_PER_SEC_OFFSET..BLOCK_ALIGN_OFFSET]
        .copy_from_slice(&avg_bytes_per_sec.to_le_bytes());
    bytes[BLOCK_ALIGN_OFFSET..BITS_PER_SAMPLE_OFFSET]
        .copy_from_slice(&params.block_align().to_le_bytes());
    bytes[BITS_PER_SAMPLE_OFFSET..DATA_ID_OFFSET]
        .copy_from_slice(&params.bits_per_sample.to_le_bytes());
    // WAV_HEADER_LEN is 44; the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    let data_len = file_pos.wrapping_sub(WAV_HEADER_LEN as u32);
    bytes[DATA_SIZE_OFFSET..WAV_HEADER_LEN].copy_from_slice(&data_len.to_le_bytes());
    bytes
}

/// The `-w` automatic filename: `AirSpy_%sZ_%ukHz_IQ.wav` with the
/// `%Y%m%d_%H%M%S` local time and `freq_hz / 1000` (`airspy_rx.c`
/// `main()`).
pub fn wav_filename(date_time: &str, freq_hz: u32) -> String {
    format!("AirSpy_{date_time}Z_{}kHz_IQ.wav", freq_hz / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
