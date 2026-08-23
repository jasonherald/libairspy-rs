# libairspy-rs

Pure-Rust port of [libairspy] — USB driver for the Airspy R2 / Mini
software-defined radio receivers, with no C library dependency.

A faithful port: USB wire behavior, sample-pipeline semantics, and DSP
output match the original C library, behind an idiomatic Rust surface —
RAII device handles, `Result`-based errors, typed enums, and
`Iterator`/`Stream`-based sample delivery. Async integrations for tokio
and smol are opt-in via feature flags; the sync path needs no features.

**Under construction** — the conversion is tracked in
[milestones](https://github.com/jasonherald/libairspy-rs/milestones).

License: BSD-3-Clause, matching upstream libairspy. See the repository
[NOTICE](https://github.com/jasonherald/libairspy-rs/blob/main/NOTICE)
for upstream attribution.

[libairspy]: https://github.com/airspy/airspyone_host
