# libairspy-rs

Pure-Rust port of [airspyone_host](https://github.com/airspy/airspyone_host) —
USB driver and CLI tools for the Airspy R2 / Mini software-defined radio
receivers, with no C library dependency (USB access via [`rusb`]).

> **Status: under construction.** The C→Rust conversion is tracked in
> [milestones](https://github.com/jasonherald/libairspy-rs/milestones) —
> scaffold → device core → streaming → IQ conversion DSP → control
> surface → tools → hardware validation. Releases are marked by
> crates.io publishes; nothing is published yet.

## Workspace layout

| Crate | Package | License | What |
|---|---|---|---|
| `crates/libairspy` | [`libairspy-rs`] | BSD-3-Clause | The driver library: device management, vendor requests, bulk streaming, IQ conversion |
| `crates/airspy-tools` | `airspy-tools` | GPL-2.0-or-later | CLI tools: `airspy_info`, `airspy_rx`, GPIO/register/flash utilities |

The two-crate split mirrors the upstream C tree's license boundary:
libairspy is BSD-3-Clause, airspy-tools are GPL. See [NOTICE](NOTICE)
for upstream attribution.

## Design goals

- **Faithful port.** Same USB wire behavior, same sample-pipeline
  semantics, DSP output validated against golden vectors generated from
  the C converters (bit-exact for int16, bounded tolerance for float32).
- **Idiomatic surface.** RAII device handles instead of open/close
  pairs, `Result` instead of int codes, typed enums instead of raw
  bytes.
- **Sync first-class, async opt-in.** Callback and `Iterator` sample
  delivery with no default dependencies; `tokio` and `smol` feature
  flags add `Stream` adapters (same pattern as [`librtlsdr-rs`]).
- **No `unsafe`.** `unsafe_code = "deny"` workspace-wide.

## Building

```sh
cargo build            # default features (sync only)
cargo build --all-features
cargo test
```

Requires `libusb-1.0` development headers (`libusb-1.0-0-dev` on
Debian/Ubuntu) for `rusb`.

## Contributing

Protected `main`; all changes land via feature-branch pull requests,
reviewed by CodeRabbit, Codacy, and a human. CI enforces rustfmt,
clippy (`-D warnings`), tests, cargo-deny, cargo-audit, and CodeQL.

## License

- `libairspy-rs` crate: [BSD-3-Clause](crates/libairspy/LICENSE)
- `airspy-tools` crate: [GPL-2.0-or-later](crates/airspy-tools/LICENSE)

[`rusb`]: https://crates.io/crates/rusb
[`libairspy-rs`]: https://crates.io/crates/libairspy-rs
[`librtlsdr-rs`]: https://github.com/jasonherald/librtlsdr-rs
