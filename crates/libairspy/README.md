# libairspy-rs

Pure-Rust port of [libairspy] — USB driver for the Airspy R2 / Mini
software-defined radio receivers, with no C library dependency.

A faithful port: USB wire behavior, sample-pipeline semantics, and DSP
output match the original C library (the IQ converters are bit-exact
against golden vectors from the C implementation), behind an idiomatic
Rust surface — RAII device handles, `Result`-based errors, typed enums,
and callback/`Iterator`/`Stream` sample delivery. Async integrations
for tokio and smol are opt-in via feature flags; the sync path needs no
features and no `unsafe`.

Validated against real hardware: on an Airspy R2 the device-gated test
suite passes 8/8, sustaining 9.965 MSPS with zero dropped samples.
Genuine upstream bugs are fixed rather than propagated, each deviation
documented at its code site.

License: `BSD-3-Clause AND MIT` — BSD-3-Clause matching upstream
libairspy, MIT for the IQ converters ported from the last all-MIT
upstream revision. See the repository
[NOTICE](https://github.com/jasonherald/libairspy-rs/blob/main/NOTICE)
for upstream attribution.

[libairspy]: https://github.com/airspy/airspyone_host
