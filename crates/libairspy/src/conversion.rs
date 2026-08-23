//! Sample-format conversion, starting with the 12-bit packed-mode
//! unpacker (`unpack_samples` in airspy.c). The IQ converters land
//! here with the DSP milestone.

/// Words consumed per unpacked group (`i += 3` in C's loop).
const WORDS_PER_GROUP: usize = 3;
/// Samples produced per group (`j += 8`).
const SAMPLES_PER_GROUP: usize = 8;

/// Unpack 12-bit packed samples: each group of three 32-bit words
/// yields eight 12-bit samples, bit layout transcribed from
/// `unpack_samples` (airspy.c). Words are the native little-endian
/// loads of the USB byte stream, as on the C library's supported
/// hosts.
///
/// Returns the number of samples written. Deviation from C: only
/// complete 3-word/8-sample groups are processed. C's loop runs to a
/// `sample_count` that is not a multiple of 8 (262144-byte buffers
/// give 174762) and overruns both buffers on the final partial group
/// — an upstream out-of-bounds this port does not reproduce.
// Consumed by the sample-type pipeline (#12); until that lands the
// unpacker has no callers outside its tests.
#[allow(dead_code)]
// Every assembled value is at most 12 bits ((0xFF << 4) | 0xF =
// 0xFFF), but clippy cannot prove it through the shift-or pairs.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn unpack_samples(input: &[u32], output: &mut [u16]) -> usize {
    let groups = core::cmp::min(
        input.len() / WORDS_PER_GROUP,
        output.len() / SAMPLES_PER_GROUP,
    );
    for (words, samples) in input
        .chunks_exact(WORDS_PER_GROUP)
        .zip(output.chunks_exact_mut(SAMPLES_PER_GROUP))
        .take(groups)
    {
        // Bit expressions transcribed 1:1 from the C loop body.
        samples[0] = ((words[0] >> 20) & 0xFFF) as u16;
        samples[1] = ((words[0] >> 8) & 0xFFF) as u16;
        samples[2] = (((words[0] & 0xFF) << 4) | ((words[1] >> 28) & 0xF)) as u16;
        samples[3] = ((words[1] & 0x0FFF_0000) >> 16) as u16;
        samples[4] = ((words[1] & 0xFFF0) >> 4) as u16;
        samples[5] = (((words[1] & 0xF) << 8) | ((words[2] & 0xFF00_0000) >> 24)) as u16;
        samples[6] = ((words[2] >> 12) & 0xFFF) as u16;
        samples[7] = (words[2] & 0xFFF) as u16;
    }
    groups * SAMPLES_PER_GROUP
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only inverse of the unpacker: pack eight 12-bit samples
    /// into three words exactly as the firmware lays them out
    /// (derived by inverting the C bit expressions).
    fn pack_group(s: [u16; 8]) -> [u32; 3] {
        let s: Vec<u32> = s.iter().map(|&v| u32::from(v & 0x0FFF)).collect();
        [
            (s[0] << 20) | (s[1] << 8) | (s[2] >> 4),
            ((s[2] & 0xF) << 28) | (s[3] << 16) | (s[4] << 4) | (s[5] >> 8),
            ((s[5] & 0xFF) << 24) | (s[6] << 12) | s[7],
        ]
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
        let full = pack_group([9, 8, 7, 6, 5, 4, 3, 2]);
        // Two extra words: not enough for another group.
        let input = [full[0], full[1], full[2], 0xDEAD_BEEF, 0x1234_5678];
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
        assert_eq!(unpack_samples(&[1, 2, 3], &mut []), 0);
    }

    #[test]
    fn all_ones_and_all_zeros_survive() {
        let ones = [0xFFF_u16; 8];
        let input = pack_group(ones);
        assert_eq!(input, [u32::MAX; 3]); // fully-set words
        let mut output = [0u16; 8];
        unpack_samples(&input, &mut output);
        assert_eq!(output, ones);

        let mut output = [0xBEEF_u16; 8];
        unpack_samples(&[0u32; 3], &mut output);
        assert_eq!(output, [0u16; 8]);
    }
}
