//! Shared logic for the `airspy_calibrate` binary, ported from
//! `airspy-tools/src/airspy_calibrate.c`: the flash calibration
//! record layout, its (de)serialization, and the timestamp
//! formatting. The binary holds the C print formats and the
//! device wiring.

use chrono::{Datelike, Timelike};

/// `AIRSPY_FLASH_CALIB_OFFSET` in `airspy_calibrate.c` — "After
/// 128KB (Reserved for Firmware + 64KB Spare)".
pub const CALIB_OFFSET: u32 = 0x20000;
/// `AIRSPY_FLASH_CALIB_HEADER` in `airspy_calibrate.c`.
pub const CALIB_HEADER: u32 = 0xCA1B_0001;
/// `sizeof(airspy_calib_t)` — three packed 32-bit fields.
pub const CALIB_LEN: usize = 12;
/// The calibration sector — the literal `2` in
/// `airspy_spiflash_erase_sector(device, 2)` and the "Erasing
/// sector 2 (calibration)" message (`airspy_calibrate.c`).
pub const CALIB_SECTOR: u16 = 2;

/// `airspy_calib_t` in `airspy_calibrate.c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calib {
    /// Shall equal [`CALIB_HEADER`].
    pub header: u32,
    /// Epoch Unix timestamp.
    pub timestamp: u32,
    /// The frequency correction in parts per billion.
    pub correction_ppb: i32,
}

impl Calib {
    /// Decode the 12 flash bytes (C reads the packed struct's raw
    /// memory; little-endian on every supported target).
    pub fn from_le_bytes(raw: &[u8; CALIB_LEN]) -> Self {
        let word = |i: usize| {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&raw[i..i + 4]);
            bytes
        };
        Self {
            header: u32::from_le_bytes(word(0)),
            timestamp: u32::from_le_bytes(word(4)),
            correction_ppb: i32::from_le_bytes(word(8)),
        }
    }

    /// Encode the 12 flash bytes.
    pub fn to_le_bytes(&self) -> [u8; CALIB_LEN] {
        let mut raw = [0u8; CALIB_LEN];
        raw[0..4].copy_from_slice(&self.header.to_le_bytes());
        raw[4..8].copy_from_slice(&self.timestamp.to_le_bytes());
        raw[8..12].copy_from_slice(&self.correction_ppb.to_le_bytes());
        raw
    }

    /// Whether the magic matches. Deviation: C displays whatever it
    /// read; the binary warns when this is false (erased flash reads
    /// as all 0xFF).
    pub fn header_valid(&self) -> bool {
        self.header == CALIB_HEADER
    }
}

/// The `%04d/%02d/%02d %02d:%02d:%02d` timestamp format shared by
/// the read and write paths of `airspy_calibrate.c` (fed from
/// `localtime`).
pub fn format_calib_time(dt: &(impl Datelike + Timelike)) -> String {
    format!(
        "{:04}/{:02}/{:02} {:02}:{:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calib_args::calib_command;

    #[test]
    fn constants_match_c_defines() {
        // airspy_calibrate.c: "After 128KB (Reserved for Firmware +
        // 64KB Spare)" and the CA1B magic.
        assert_eq!(CALIB_OFFSET, 0x20000);
        assert_eq!(CALIB_HEADER, 0xCA1B_0001);
        assert_eq!(CALIB_LEN, 12);
        assert_eq!(CALIB_SECTOR, 2);
    }

    #[test]
    fn calib_round_trips_little_endian() {
        // airspy_calib_t: uint32 header, uint32 timestamp, int32
        // correction_ppb — 12 packed bytes, little-endian on the
        // wire and in flash.
        let calib = Calib {
            header: CALIB_HEADER,
            timestamp: 1_766_620_800,
            correction_ppb: -1500,
        };
        let bytes = calib.to_le_bytes();
        assert_eq!(bytes.len(), CALIB_LEN);
        assert_eq!(bytes[0..4], CALIB_HEADER.to_le_bytes());
        assert_eq!(Calib::from_le_bytes(&bytes), calib);
    }

    #[test]
    fn header_validity_flags_unprogrammed_flash() {
        // Deviation: C displays whatever it read (erased flash is all
        // 0xFF, so garbage timestamps print); the port warns when the
        // magic does not match.
        let good = Calib {
            header: CALIB_HEADER,
            timestamp: 0,
            correction_ppb: 0,
        };
        assert!(good.header_valid());
        let erased = Calib::from_le_bytes(&[0xFF; CALIB_LEN]);
        assert!(!erased.header_valid());
    }

    #[test]
    fn timestamp_formats_like_c_localtime_printf() {
        // The %04d/%02d/%02d %02d:%02d:%02d format in
        // airspy_calibrate.c (both the read and write paths).
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 8, 25)
            .expect("date")
            .and_hms_opt(7, 5, 9)
            .expect("time");
        assert_eq!(format_calib_time(&dt), "2026/08/25 07:05:09");
    }

    #[test]
    fn command_mirrors_c_getopt_flags() {
        // getopt(argc, argv, "rw:") — no serial option in this tool;
        // --force is the calibration-write confirmation deviation.
        let matches = calib_command()
            .try_get_matches_from(["t", "-r"])
            .expect("parse");
        assert!(matches.get_flag("read"));
        let matches = calib_command()
            .try_get_matches_from(["t", "-w", "-1500", "--force"])
            .expect("parse");
        assert_eq!(matches.get_one::<i32>("write"), Some(&-1500));
        assert!(matches.get_flag("force"));
        assert!(
            calib_command()
                .try_get_matches_from(["t", "-s", "0x1"])
                .is_err()
        );
        // Deviation: C's atoi silently turns garbage into 0 (and
        // "12abc" into 12); a value bound for flash parses strictly.
        assert!(
            calib_command()
                .try_get_matches_from(["t", "-w", "abc"])
                .is_err()
        );
        assert!(
            calib_command()
                .try_get_matches_from(["t", "-w", "12abc"])
                .is_err()
        );
    }
}
