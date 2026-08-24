//! Half-band FIR kernels transcribed from `filters.h` of
//! `airspyone_host` at the all-MIT reference revision `bd15be38`
//! (MIT License, Copyright (C) 2014, Youssef Touil — see NOTICE).

/// `HB_KERNEL_INT16_LEN` — `#define HB_KERNEL_INT16_LEN 47`
/// (`filters.h`, `airspyone_host` @ `bd15be38`).
pub(crate) const HB_KERNEL_INT16_LEN: usize = 47;

/// `HB_KERNEL_INT16` (filters.h) — Q15 half-band low-pass taps; every
/// odd tap is zero apart from the center (16384 = 0.5 in Q15).
#[rustfmt::skip]
pub(crate) const HB_KERNEL_INT16: [i16; HB_KERNEL_INT16_LEN] = [
    -33, 0, 56, 0, -100, 0, 166, 0, -259, 0, 389, 0, -571, 0, 829, 0,
    -1220, 0, 1885, 0, -3353, 0, 10389, 16384, 10389, 0, -3353, 0,
    1885, 0, -1220, 0, 829, 0, -571, 0, 389, 0, -259, 0, 166, 0,
    -100, 0, 56, 0, -33,
];
