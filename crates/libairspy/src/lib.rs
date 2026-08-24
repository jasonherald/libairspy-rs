//! Pure-Rust port of [libairspy] — USB driver for the Airspy R2 / Mini
//! software-defined radio receivers, with no C library dependency.
//!
//! This is a faithful port: USB wire behavior, sample-pipeline
//! semantics, and DSP output match the original C library, behind an
//! idiomatic Rust surface (RAII device handles, `Result`-based errors,
//! typed enums, and `Iterator`/`Stream`-based sample delivery).
//!
//! The crate is under active construction; the conversion is tracked in
//! [milestones on GitHub](https://github.com/jasonherald/libairspy-rs/milestones).
//!
//! [libairspy]: https://github.com/airspy/airspyone_host
#![warn(missing_docs)]

pub mod board;
pub mod commands;
pub mod conversion;
pub mod device;
pub mod error;
pub mod reader;
pub mod stream;
mod transfer;

pub use board::PartIdSerial;
pub use conversion::Samples;
pub use device::{Device, list_devices};
pub use error::{Error, Result};

/// Library version, mirroring `airspy_lib_version` /
/// `airspy_lib_version_t` — reporting this crate's own version rather
/// than the C library's 1.0.12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LibVersion {
    /// `major_version`
    pub major: u32,
    /// `minor_version`
    pub minor: u32,
    /// `revision`
    pub revision: u32,
}

/// The version of this crate (`airspy_lib_version` semantics).
#[must_use]
pub fn lib_version() -> LibVersion {
    let parse = |s: &str| s.parse().unwrap_or(0);
    LibVersion {
        major: parse(env!("CARGO_PKG_VERSION_MAJOR")),
        minor: parse(env!("CARGO_PKG_VERSION_MINOR")),
        revision: parse(env!("CARGO_PKG_VERSION_PATCH")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lib_version_matches_crate_version() {
        let v = lib_version();
        assert_eq!(
            format!("{}.{}.{}", v.major, v.minor, v.revision),
            env!("CARGO_PKG_VERSION")
        );
    }
}
