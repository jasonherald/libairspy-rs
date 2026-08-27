# libairspy-rs

[![Codacy Badge](https://app.codacy.com/project/badge/Grade/76a82bc8c8124d67abc41efb5dd2b63b)](https://app.codacy.com/gh/jasonherald/libairspy-rs/dashboard?utm_source=gh&utm_medium=referral&utm_content=&utm_campaign=Badge_grade)
[![Codacy Badge](https://app.codacy.com/project/badge/Coverage/76a82bc8c8124d67abc41efb5dd2b63b)](https://app.codacy.com/gh/jasonherald/libairspy-rs/dashboard?utm_source=gh&utm_medium=referral&utm_content=&utm_campaign=Badge_coverage)
[![crates.io](https://img.shields.io/crates/v/libairspy-rs.svg)](https://crates.io/crates/libairspy-rs)
[![docs.rs](https://img.shields.io/docsrs/libairspy-rs)](https://docs.rs/libairspy-rs)

Pure-Rust port of [airspyone_host](https://github.com/airspy/airspyone_host) —
USB driver and CLI tools for the Airspy R2 / Mini software-defined radio
receivers, with no C library dependency (USB access via [`rusb`]).

The port covers all implemented upstream functionality — the full
`libairspy` control/streaming surface and all eight CLI tools — and is
**validated against real hardware**: on an Airspy R2, the device test
suite passes 8/8 with 9.965 MSPS sustained and zero dropped samples,
and every tool's read output is byte-identical to C `airspy 1.0.10`.
See [CHANGELOG.md](CHANGELOG.md) for the release history.

## Workspace layout

| Crate | Package | License | What |
|---|---|---|---|
| `crates/libairspy` | [`libairspy-rs`] | BSD-3-Clause AND MIT | The driver library: device management, vendor requests, bulk streaming, IQ conversion |
| `crates/airspy-tools` | `airspy-tools` (unpublished) | GPL-2.0-or-later | The eight CLI tools: `airspy_info`, `airspy_rx`, `airspy_gpio`, `airspy_gpiodir`, `airspy_si5351c`, `airspy_r820t`, `airspy_spiflash`, `airspy_calibrate` |

The two-crate split mirrors the upstream C tree's license boundary:
libairspy is BSD-3-Clause (with the IQ converters ported from the last
all-MIT upstream revision, hence `AND MIT`), airspy-tools are GPL. See
[NOTICE](NOTICE) for upstream attribution.

## Design goals

- **Faithful port.** Same USB wire behavior, same sample-pipeline
  semantics, DSP output validated against golden vectors generated from
  the C converters (bit-exact for both int16 and float32). Tool output
  is diffable against the C tools.
- **More correct than upstream where it matters.** Genuine upstream
  bugs (out-of-bounds accesses, silent numeric truncations, option
  parsing mistakes) are fixed rather than propagated — each deviation
  documented at its code site and in the changelog. Destructive tool
  operations additionally require an explicit `--force`.
- **Idiomatic surface.** RAII device handles instead of open/close
  pairs, `Result` instead of int codes, typed enums instead of raw
  bytes.
- **Sync first-class, async opt-in.** Callback and `Iterator` sample
  delivery with no default dependencies; `tokio` and `smol` feature
  flags add `Stream` adapters (same pattern as [`librtlsdr-rs`]).
- **No `unsafe` in the library.** `unsafe_code = "deny"`
  workspace-wide; the tools crate carries a single documented
  exception (`signal(SIGPIPE, SIG_DFL)` for C-parity pipe behavior).

## Building

```sh
cargo build            # default features (sync only)
cargo build --all-features
cargo test
```

Requires `libusb-1.0` development headers (`libusb-1.0-0-dev` on
Debian/Ubuntu) for `rusb`.

With an Airspy attached, the device-gated integration tests run via:

```sh
cargo test -p libairspy-rs --test device --release -- --ignored --test-threads=1
```

## Contributing

Protected `main`; all changes land via feature-branch pull requests,
reviewed by CodeRabbit, Codacy, and a human. CI enforces rustfmt,
clippy (`-D warnings`), tests, cargo-deny, cargo-audit, and CodeQL.

## License

- `libairspy-rs` crate: [BSD-3-Clause AND MIT](crates/libairspy/LICENSE)
- `airspy-tools` crate: [GPL-2.0-or-later](crates/airspy-tools/LICENSE)

[`rusb`]: https://crates.io/crates/rusb
[`libairspy-rs`]: https://crates.io/crates/libairspy-rs
[`librtlsdr-rs`]: https://github.com/jasonherald/librtlsdr-rs
