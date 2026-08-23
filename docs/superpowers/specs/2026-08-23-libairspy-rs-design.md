# libairspy-rs — Design Spec

Date: 2026-08-23
Status: Approved pending review

## Purpose

Port the Airspy host support (libairspy + airspy-tools, C) to pure Rust,
producing a `libairspy-rs` crate that the `rtl-sdr` SDR application
workspace can consume as a device source — following the exact pattern
`librtlsdr-rs` established (standalone repo, published to crates.io,
consumed via a thin `sdr-source-*` crate in the app workspace).

The immediate driver: an Airspy R2 is arriving; the port and the
hardware validate each other.

## Source material

The original C code lives at `original/airspyone_host/` in this working
directory. It is reference material only and is **gitignored — it must
never be committed**.

- `libairspy/src/` (~3,000 lines, BSD-3-Clause):
  - `airspy.c` (2,011 lines) — USB device management, vendor requests,
    bulk streaming, sample pipeline dispatch
  - `airspy.h` — public API (~60 functions)
  - `airspy_commands.h` — vendor-request command enums
  - `iqconverter_float.c` / `iqconverter_int16.c` — IQ conversion DSP
    (half-band FIR + delay-line Hilbert-style translation)
  - `filters.h` — FIR kernel constants
- `airspy-tools/src/` (GPL-2.0-or-later): `airspy_rx`, `airspy_info`,
  `airspy_gpio`, `airspy_gpiodir`, `airspy_si5351c`, `airspy_r820t`,
  `airspy_spiflash`, `airspy_calibrate`

## Repo & workspace layout

GitHub: `jasonherald/libairspy-rs`, public. Local checkout is this
directory (`/data/source/airspy`, also reachable as `~/source/airspy` —
same directory via symlink).

Cargo workspace, two crates, matching the license boundary of the
original code:

| Crate dir | Package | License | Contents |
|---|---|---|---|
| `crates/libairspy` | `libairspy-rs` | BSD-3-Clause | Library port of libairspy; published to crates.io |
| `crates/airspy-tools` | `airspy-tools` | GPL-2.0-or-later | The 8 CLI tools, clap-based; not published initially |

Root files mirrored from `librtlsdr-rs` / `rtl-sdr`:
`rust-toolchain.toml` (stable pin), `deny.toml`, `NOTICE` (upstream
copyright attribution for Ossmann/Boone/Touil/Vernoux/Gilmour),
`CHANGELOG.md`, `SECURITY.md`, per-crate `LICENSE` files.

Workspace conventions (same as rtl-sdr): edition 2024, resolver 3,
workspace lints — `unsafe_code = "deny"` (narrow, documented allows only
if rusb forces it), clippy `all` + `pedantic` warn,
`unwrap_used`/`panic` warn, release profile with LTO +
`codegen-units = 1` + strip.

## Library architecture (`libairspy-rs`)

Faithful port: same USB wire behavior, same sample-pipeline semantics,
idiomatic Rust surface (RAII instead of open/close pairs, `Result`
instead of int codes, typed enums instead of `uint8_t`).

- `error.rs` — `thiserror` enum mirroring `enum airspy_error`
- `commands.rs` — vendor-request constants from `airspy_commands.h`,
  board-id and sample-type enums
- `device.rs` — enumeration (`list_devices`), open by serial / first,
  `Drop`-based close, board id / version string / partid-serial reads,
  and the control surface: `set_freq`, `set_samplerate` (+
  `get_samplerates`), LNA/mixer/VGA gains, LNA/mixer AGC,
  linearity/sensitivity composite gains, RF bias, packing, sample type,
  custom conversion filter kernels
- `stream.rs` — bulk-transfer worker thread; sync API first-class
  (callback matching `airspy_start_rx` semantics, plus a safe
  `Iterator`-based reader); `is_streaming` / stop semantics identical to
  the C library
- Async adapters, from the start, mirroring `librtlsdr-rs` exactly:
  `tokio` feature (`tokio::task::spawn_blocking` + `tokio::sync::mpsc`,
  `futures-core` `Stream`) and `smol` feature (`blocking::unblock` +
  `async-channel`). Default features empty; sync path needs none.
- `conversion/` — 12-bit sample unpacking, the six sample types
  (float32/int16 × IQ/real, uint16 real, raw), ports of
  `iqconverter_float` and `iqconverter_int16`, `filters.rs` kernels
- Peripheral access (thin typed wrappers, required by the tools):
  `si5351c` read/write, `r820t` read/write, GPIO + GPIO direction,
  SPI flash (erase/erase-sector/read/write), config pages (read/write)

Dependencies: `rusb`, `thiserror`, `tracing`; optional `tokio`,
`futures-core`, `async-channel`, `blocking` behind the two async
features. Dev: golden-vector test helpers only (no heavy frameworks).

## Testing strategy

- **DSP golden vectors**: compile the C IQ converters locally (throwaway
  harness under scratch space, never committed) to generate reference
  input/output vectors; commit the small vector files under
  `test-data/`; Rust converters must match bit-for-bit (int16) / within
  documented tolerance (float32).
- **Packing/unpacking**: pure unit tests with hand-built patterns.
- **Control/USB paths**: unit-test the request encoding where possible;
  device-dependent integration tests are `#[ignore]`d and gated behind
  hardware presence until the R2 arrives (M6 flips them on).
- CI runs `cargo test` for default features and each async feature
  combination, matching librtlsdr-rs's "Feature combinations" jobs.

## GitHub / CI / review tooling

- **Branch protection**: clone the `librtlsdr-rs` "Primary" **ruleset**
  onto `main` — PR required, required review-thread resolution,
  last-push approval, extra approval for unattributed changes, no
  deletion / force-push, strict required status checks (contexts
  adjusted to this repo's job names), code-quality severity `warnings`.
- **Workflows** (adapted from librtlsdr-rs/rtl-sdr): `ci.yml` (fmt,
  clippy `-D warnings`, build, test, cargo-deny, cargo-audit, tokio and
  smol feature-combo jobs, docs.rs config check), `codeql.yml`,
  `audit.yml` (scheduled), `deny.yml`. Only system dep:
  `libusb-1.0-0-dev`.
- **CodeRabbit** (established tooling — in use across rtl-sdr for a long
  time): `.coderabbit.yaml` adapted from rtl-sdr's — same assertive
  profile, tools (clippy, gitleaks, markdownlint, yamllint, actionlint),
  knowledge-base settings; `path_instructions` rewritten for this repo:
  USB register/command exactness vs the C source, DSP numerical
  accuracy, no hot-path allocations, streaming thread-safety; GTK
  sections dropped. `path_filters` exclude `original/**` and
  `target/**`.
- **Codacy** (new tooling for us — rtl-sdr's config is itself brand
  new): do NOT copy rtl-sdr's generated 200KB baseline. Register this
  repo on Codacy Cloud after the scaffold exists, then generate and
  tune `.codacy/` fresh against the real code via the Codacy
  skills/CLI.
- **Git flow**: protected `main`, feature branches, PRs reviewed by
  Jason + CodeRabbit + Codacy. Conventional, small PRs — one issue per
  PR where practical.

## Work control: milestones, epics, issues

Each milestone is a GitHub milestone plus one epic issue carrying a task
list that links child issues. Child issues are scoped to be one-PR-sized.

- **M0 Scaffold** — workspace + both crate skeletons, CI workflows,
  `.coderabbit.yaml`, deny/audit/toolchain configs, licenses + NOTICE,
  README, `error.rs` + `commands.rs` type foundations, branch ruleset,
  Codacy registration.
- **M1 Device core** — rusb enumeration/open/close, vendor-request
  plumbing, board id / version / partid-serial reads; **`airspy_info`
  ported early** as the end-to-end hardware smoke test.
- **M2 Streaming** — bulk transfer engine, 12-bit unpacking, sample-type
  plumbing, sync callback + iterator APIs, tokio + smol adapters,
  feature-combo CI jobs.
- **M3 IQ conversion DSP** — `iqconverter_float`, `iqconverter_int16`,
  filter kernels, golden-vector generation + tests.
- **M4 Control surface** — freq/samplerate/gain/AGC/bias/packing
  setters; si5351c, r820t, GPIO, SPI-flash, config-page wrappers.
- **M5 Tools** — remaining 7 CLIs in `airspy-tools`.
- **M6 Hardware validation & release** — R2-in-hand test pass across
  sample rates/types/gains, un-`#[ignore]` device tests, `v0.1.0`
  publish of `libairspy-rs` to crates.io.

Sequencing note: M1's `airspy_info` needs a sliver of the tools crate
early; that is intentional (hardware smoke test), the rest of the tools
wait for M5.

## Out of scope (YAGNI)

- Airspy HF+ / Mini-specific handling beyond what libairspy itself does
  (board-id enum carries them; no extra code paths).
- `airspy_open_fd` (Android file-descriptor path) — not applicable.
- A `sdr-source-airspy` crate for the rtl-sdr app workspace — that is a
  follow-on project in the rtl-sdr repo once this crate is on crates.io.
- Firmware (airspyone_firmware) — host side only.
