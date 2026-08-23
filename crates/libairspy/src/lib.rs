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

pub mod commands;
pub mod device;
pub mod error;

pub use device::{Device, list_devices};
pub use error::{Error, Result};
