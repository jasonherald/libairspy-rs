# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(pre-1.0: breaking changes bump the minor version). Releases are marked
by crates.io publishes of `libairspy-rs`.

## [Unreleased]

## [0.2.1] - 2026-09-20

Driver refinements from the #74 review (deferred from 0.2.0 as issue #75).
These change USB init/streaming behavior, so each is **re-validated on real
Airspy hardware before this version is published**. No API changes.

### Changed

- **`open_serial` / `list_devices` now read the serial from the device's
  string descriptor** (opening each candidate, as the C library does)
  instead of trusting nusb's enumeration-time `DeviceInfo::serial_number`,
  which can be `None` for a present device — previously dropping a
  supported Airspy and making `open_serial` return `NotFound`.
- **Endpoint-halt clears now use nusb's `Endpoint::clear_halt`**, which
  resets the host-side data toggle (a bare `CLEAR_FEATURE` control transfer
  did not). After a halt reset following bulk traffic the host/device
  toggles could otherwise diverge and stall subsequent transfers. This is
  the `libusb_clear_halt` behavior `airspy.c` relies on.
- **`start_rx` clears the halt synchronously between the receiver-mode OFF
  and RX commands** (`OFF → clear_halt → RX → threads`), matching
  `airspy_start_rx`, instead of deferring it into the reader thread.

### Fixed

- **A persistent bulk-transfer error storm no longer spins the reader.** On
  top of the immediate stop for unrecoverable `Stall`/`InvalidArgument`
  (0.2.0), a run of consecutive non-fatal errors with no successful
  transfer in between now stops the stream after a bounded count (with a
  short backoff between retries). A single transient fault is still
  tolerated — the run resets on the next good transfer.
- **Pool-exhaustion drops** were already counted in 0.2.0; unchanged here.

## [0.2.0] - 2026-09-19

Minor bump (pre-1.0 breaking, per this file's versioning note): the USB
backend swap changes the error types carried by `Error`.

### Changed

- **USB backend migrated from `rusb` (libusb) to `nusb` 0.2** — a pure-Rust,
  no-C-dependency async USB stack. The streaming reader now keeps a pool of
  16 bulk transfers in flight (matching the C `airspyone_host` ring) via
  nusb's `Endpoint` submit / `wait_next_complete` / resubmit loop, replacing
  the previous single synchronous `read_bulk`. The device/control/streaming
  method surface (`open`, `start_rx`, etc.) is unchanged.
- **BREAKING:** `Error::Usb` now wraps `nusb::Error` (was `rusb::Error`), and
  a new `Error::Transfer(nusb::transfer::TransferError)` variant carries
  control-transfer failures (bulk-completion errors are handled inside the
  streaming worker — dropped, or stopped on an unrecoverable status — and
  are not returned to callers). Both keep the `AIRSPY_ERROR_LIBUSB (-1000)`
  code/name. Downstreams that matched on the inner error type must update.

### Fixed

- **Stream starvation and periodic `EIO`/`Fault` under load.** The old
  single-transfer reader starved the USB pipe between reads, dropping the
  stream on newer kernels. The 16-in-flight pool keeps the pipe fed, and the
  reader now tolerates transient bulk-transfer errors (drops the buffer,
  resubmits, and keeps streaming), stopping only on a genuine device
  disconnect rather than aborting on the first non-timeout error.

## [0.1.0] - 2026-08-27

First release: a pure-Rust port of all implemented `airspyone_host`
functionality — library and all eight tools — validated against an
Airspy R2.

### Added

- **`libairspy-rs` library** (license `BSD-3-Clause AND MIT`), a
  faithful wire-compatible port of `libairspy`:
  - Device enumeration, open (plain and by 64-bit serial), and the
    `airspy.h` control surface (complete except the
    header-only `airspy_config_read`/`write` — see Changed):
    frequency, sample rate
    (index-or-Hz), individual LNA/mixer/VGA gains, linearity and
    sensitivity composite gain tables, AGCs, bias tee, 12-bit sample
    packing, receiver mode.
  - Board reads: board id, firmware version string, part id + serial.
  - Peripheral access: si5351c and R820T register read/write, the
    GPIO/GPIO-direction surface, SPI flash read/write/erase.
  - Streaming: the C 8-buffer ring as a sync `read_bulk` reader plus
    consumer thread; callback API (`start_rx`), blocking iterator
    (`rx_blocks`), and feature-gated `tokio`/`smol` adapters.
  - Bit-exact IQ converters ported from the last all-MIT upstream
    revision (`bd15be38`), golden-vector tested against the C
    implementation (int16 and float32 both bit-exact).
  - A mockable `UsbTransport` seam: the entire control and streaming
    surface is unit-tested without hardware (wire-level
    request/response assertions against independently transcribed
    constants).
  - Device-gated integration tests (`--ignored`) covering identity,
    the control surface, every sample type, packed capture, sustained
    zero-drop throughput, and stream restarts.
- **`airspy-tools`** (GPL-2.0-or-later, unpublished): ports of all
  eight CLIs — `airspy_info`, `airspy_rx` (incl. SDR#-compatible WAV
  output), `airspy_gpio`, `airspy_gpiodir`, `airspy_si5351c`,
  `airspy_r820t`, `airspy_spiflash`, `airspy_calibrate` — with
  C-diffable output and C-semantics argument parsing.
- Workspace scaffold and CI (fmt/clippy/test, feature matrix, docs,
  MSRV 1.95, cross-platform, CodeQL, cargo-audit, cargo-deny,
  coverage), licensing and upstream attribution.

### Changed

Deviations from the C implementation, each documented at its code
site. Upstream bugs are fixed rather than propagated:

- Converter out-of-bounds accesses (packed-tail unpack,
  `translate_fs_4`) and the half-uninitialized int16 converter reset.
- `airspy_rx`'s `-b` serial-number copy-paste bug.
- WAV `dwAvgBytesPerSec` missing the channel factor, and the WAV
  size-field wrap past 4 GiB (captures stop at the RIFF limit).
- Flash bounds checks that ignored the address.
- `strtoul`/`atoi` silent truncations on values bound for hardware.
- Register writes to C's unset-selection sentinel registers.
- Destructive tool operations (`airspy_gpiodir -w`, `airspy_spiflash
  -w`, `airspy_calibrate -w`) require an explicit `--force`.
- Failed tool operations exit nonzero (C exits 0).
- `airspy_config_read`/`airspy_config_write` are omitted: they exist
  only as header declarations upstream, with no implementation.

### Validated

- Airspy R2 (firmware `AirSpy NOS v1.0.0-rc10`): 8/8 device-gated
  tests; 9.965 MSPS sustained with zero dropped samples; every tool
  read path byte-identical to C `airspy 1.0.10`; gain chain verified
  (+55.8 dB across the linearity range); LED/register/bias-tee write
  round-trips confirmed.
