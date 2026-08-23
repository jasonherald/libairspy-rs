//! Shared helpers for the airspy-tools CLI binaries.
//!
//! The binaries themselves (`airspy_info`, `airspy_rx`, …) are added as
//! `[[bin]]` targets while the port progresses; this library target
//! holds the argument-parsing and output-formatting code they share.
#![warn(missing_docs)]
