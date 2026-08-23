//! Sample-format conversion, starting with the 12-bit packed-mode
//! unpacker (`unpack_samples` in airspy.c). The IQ converters land
//! here with the DSP milestone.

/// Bytes consumed per unpacked group — three 32-bit words (`i += 3`
/// in `unpack_samples`, airspy.c).
const BYTES_PER_GROUP: usize = 12;
/// Samples produced per group (`j += 8` in `unpack_samples`,
/// airspy.c).
const SAMPLES_PER_GROUP: usize = 8;

/// Unpack 12-bit packed samples from the raw USB byte stream: each
/// 12-byte group (three little-endian 32-bit words) yields eight
/// 12-bit samples, bit layout transcribed from `unpack_samples`
/// (airspy.c). C casts the byte buffer to `uint32_t*` and relies on a
/// little-endian host; loading through `u32::from_le_bytes` gives the
/// identical values portably and without alignment requirements.
///
/// Returns the number of samples written. Deviation from C: only
/// complete 12-byte/8-sample groups are processed. C's loop runs to a
/// `sample_count` that is not a multiple of 8 (262144-byte buffers
/// give 174762) and overruns both buffers on the final partial group
/// — an upstream out-of-bounds this port does not reproduce.
// Consumed by the sample-type pipeline (#12); until that lands the
// unpacker has no callers outside its tests.
#[allow(dead_code)]
// Every assembled value is at most 12 bits ((0xFF << 4) | 0xF =
// 0xFFF), but clippy cannot prove it through the shift-or pairs.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn unpack_samples(input: &[u8], output: &mut [u16]) -> usize {
    let groups = core::cmp::min(
        input.len() / BYTES_PER_GROUP,
        output.len() / SAMPLES_PER_GROUP,
    );
    let word = |chunk: &[u8], i: usize| -> u32 {
        // Infallible: chunks_exact guarantees 12 bytes per group.
        chunk
            .get(i * 4..i * 4 + 4)
            .and_then(|b| b.try_into().ok())
            .map_or(0, u32::from_le_bytes)
    };
    for (bytes, samples) in input
        .chunks_exact(BYTES_PER_GROUP)
        .zip(output.chunks_exact_mut(SAMPLES_PER_GROUP))
    {
        let (w0, w1, w2) = (word(bytes, 0), word(bytes, 1), word(bytes, 2));
        // Bit expressions transcribed 1:1 from the C loop body; the
        // literal masks/shifts keep the side-by-side parity audit
        // against airspy.c trivial.
        samples[0] = ((w0 >> 20) & 0xFFF) as u16;
        samples[1] = ((w0 >> 8) & 0xFFF) as u16;
        samples[2] = (((w0 & 0xFF) << 4) | ((w1 >> 28) & 0xF)) as u16;
        samples[3] = ((w1 & 0x0FFF_0000) >> 16) as u16;
        samples[4] = ((w1 & 0xFFF0) >> 4) as u16;
        samples[5] = (((w1 & 0xF) << 8) | ((w2 & 0xFF00_0000) >> 24)) as u16;
        samples[6] = ((w2 >> 12) & 0xFFF) as u16;
        samples[7] = (w2 & 0xFFF) as u16;
    }
    groups * SAMPLES_PER_GROUP
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only inverse of the unpacker: pack eight 12-bit samples
    /// into three words exactly as the firmware lays them out
    /// (derived by inverting the C bit expressions).
    fn pack_group(s: [u16; 8]) -> Vec<u8> {
        let s: Vec<u32> = s.iter().map(|&v| u32::from(v & 0x0FFF)).collect();
        let words = [
            (s[0] << 20) | (s[1] << 8) | (s[2] >> 4),
            ((s[2] & 0xF) << 28) | (s[3] << 16) | (s[4] << 4) | (s[5] >> 8),
            ((s[5] & 0xFF) << 24) | (s[6] << 12) | s[7],
        ];
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn unpacks_one_group_matching_c_bit_layout() {
        let samples = [0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x135, 0x790, 0x246];
        let input = pack_group(samples);
        let mut output = [0u16; 8];
        assert_eq!(unpack_samples(&input, &mut output), 8);
        assert_eq!(output, samples);
    }

    #[test]
    fn unpacks_hardcoded_fixture_derived_from_c() {
        // Independent of pack_group: words 0x12345678, 0x9ABCDEF0,
        // 0x13579BDF pushed through the C expressions by hand —
        //   s0=(w0>>20)&FFF=0x123      s1=(w0>>8)&FFF=0x456
        //   s2=((w0&FF)<<4)|(w1>>28)=0x789
        //   s3=(w1&0FFF0000)>>16=0xABC s4=(w1&FFF0)>>4=0xDEF
        //   s5=((w1&F)<<8)|(w2>>24)=0x013
        //   s6=(w2>>12)&FFF=0x579      s7=w2&FFF=0xBDF
        let input: [u8; 12] = [
            0x78, 0x56, 0x34, 0x12, // w0 LE
            0xF0, 0xDE, 0xBC, 0x9A, // w1 LE
            0xDF, 0x9B, 0x57, 0x13, // w2 LE
        ];
        let mut output = [0u16; 8];
        assert_eq!(unpack_samples(&input, &mut output), 8);
        assert_eq!(
            output,
            [0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x013, 0x579, 0xBDF]
        );
    }

    #[test]
    fn unpacks_multiple_groups_in_order() {
        let a = [1, 2, 3, 4, 5, 6, 7, 8];
        let b = [0xFFF, 0, 0xAAA, 0x555, 0x0F0, 0xF0F, 0x800, 0x7FF];
        let mut input = Vec::new();
        input.extend_from_slice(&pack_group(a));
        input.extend_from_slice(&pack_group(b));
        let mut output = [0u16; 16];
        assert_eq!(unpack_samples(&input, &mut output), 16);
        assert_eq!(&output[..8], &a);
        assert_eq!(&output[8..], &b);
    }

    #[test]
    fn clamps_to_complete_groups() {
        // C's loop overruns on a partial tail (sample_count 174762 is
        // not a multiple of 8 — an upstream OOB); the port processes
        // complete 3-word/8-sample groups only and reports what it
        // wrote.
        let mut input = pack_group([9, 8, 7, 6, 5, 4, 3, 2]);
        // Eight extra bytes: not enough for another 12-byte group.
        input.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x12, 0x34, 0x56, 0x78]);
        let mut output = [0u16; 16];
        assert_eq!(unpack_samples(&input, &mut output), 8);
        assert_eq!(&output[..8], &[9, 8, 7, 6, 5, 4, 3, 2]);
        // Untouched tail.
        assert_eq!(&output[8..], &[0u16; 8]);

        // Output shorter than the input allows: clamp to output groups.
        let mut short_out = [0u16; 11]; // one full group only
        assert_eq!(unpack_samples(&input, &mut short_out), 8);
    }

    #[test]
    fn empty_input_or_output_writes_nothing() {
        let mut output = [0u16; 8];
        assert_eq!(unpack_samples(&[], &mut output), 0);
        assert_eq!(unpack_samples(&[1u8; 12], &mut []), 0);
    }

    #[test]
    fn all_ones_and_all_zeros_survive() {
        let ones = [0xFFF_u16; 8];
        let input = pack_group(ones);
        assert_eq!(input, vec![0xFF; 12]); // fully-set bytes
        let mut output = [0u16; 8];
        unpack_samples(&input, &mut output);
        assert_eq!(output, ones);

        let mut output = [0xBEEF_u16; 8];
        unpack_samples(&[0u8; 12], &mut output);
        assert_eq!(output, [0u16; 8]);
    }
}
