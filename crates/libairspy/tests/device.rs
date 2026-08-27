//! Device-gated integration tests — the M6 hardware-validation suite.
//!
//! Every test here talks to a real Airspy over USB and is therefore
//! `#[ignore]`d so CI (and any hardware-free machine) stays green.
//! With an Airspy connected, run them single-threaded — the tests
//! share one physical device and each opens it exclusively:
//!
//! ```sh
//! cargo test -p libairspy-rs --test device -- --ignored --test-threads=1
//! ```
//!
//! The bias tee is deliberately never enabled: these tests cannot
//! know what is attached to the antenna port, and 4.5 V on the wrong
//! hardware is destructive.

use std::time::{Duration, Instant};

use libairspy_rs::commands::{BoardId, SampleType};
use libairspy_rs::reader::SampleBlock;
use libairspy_rs::{Device, list_devices};

/// A tuning target for the streaming tests. The content of the
/// spectrum does not matter (no antenna may be attached); 100 MHz is
/// simply a valid, mid-band frequency.
const TEST_FREQ_HZ: u32 = 100_000_000;

/// Blocks to pull for the short per-type streams — enough to cross
/// several USB transfers without dragging the suite out.
const SHORT_STREAM_BLOCKS: usize = 5;

#[test]
#[ignore = "requires Airspy hardware"]
fn enumerates_at_least_one_device() {
    let serials = list_devices().expect("list_devices");
    assert!(!serials.is_empty(), "no Airspy enumerated");
    for serial in &serials {
        println!("enumerated serial: 0x{serial:016X}");
    }
}

#[test]
#[ignore = "requires Airspy hardware"]
fn identity_reads_match_an_airspy() {
    let device = Device::open().expect("open");
    let board_id = device.board_id().expect("board_id");
    println!("board id: {board_id}");
    assert_eq!(
        BoardId::from_u8(board_id),
        Some(BoardId::ProtoAirspy),
        "unexpected board id {board_id}"
    );

    let version = device.version_string().expect("version_string");
    println!("firmware: {version}");
    assert!(!version.is_empty());
    assert!(version.contains("AirSpy"), "unexpected version: {version}");

    let ids = device.partid_serialno().expect("partid_serialno");
    println!(
        "part id: 0x{:08X} 0x{:08X}, serial: 0x{:08X}{:08X}",
        ids.part_id[0], ids.part_id[1], ids.serial_no[2], ids.serial_no[3]
    );
    assert_ne!(ids.serial_no, [0; 4], "serial reads as all zero");
}

#[test]
#[ignore = "requires Airspy hardware"]
fn samplerate_table_is_populated() {
    let device = Device::open().expect("open");
    let rates = device.samplerates();
    println!("supported IQ rates: {rates:?}");
    assert!(!rates.is_empty());
    // Every known Airspy firmware advertises 10 MSPS; the R2 also
    // lists 2.5 MSPS.
    assert!(rates.contains(&10_000_000), "10 MSPS missing: {rates:?}");
}

#[test]
#[ignore = "requires Airspy hardware"]
fn control_surface_accepts_the_full_range() {
    let mut device = Device::open().expect("open");
    device.set_samplerate(0).expect("set_samplerate index 0");
    device.set_freq(TEST_FREQ_HZ).expect("set_freq");

    // Gain endpoints (clamping covered by unit tests; here the wire
    // must accept the extremes).
    device.set_lna_gain(0).expect("lna 0");
    device.set_lna_gain(14).expect("lna max");
    device.set_mixer_gain(0).expect("mixer 0");
    device.set_mixer_gain(15).expect("mixer max");
    device.set_vga_gain(0).expect("vga 0");
    device.set_vga_gain(15).expect("vga max");
    device.set_linearity_gain(10).expect("linearity");
    device.set_sensitivity_gain(10).expect("sensitivity");
    device.set_lna_agc(false).expect("lna agc off");
    device.set_mixer_agc(false).expect("mixer agc off");
    device.set_packing(false).expect("packing off");
    // NOTE: set_rf_bias(true) is intentionally absent — see the
    // module docs. Turning it off is safe regardless.
    device.set_rf_bias(false).expect("bias off");
}

/// Pull a handful of blocks in the device's current configuration and
/// return (blocks, total dropped samples).
fn capture_blocks(device: &mut Device, count: usize) -> (Vec<SampleBlock>, u64) {
    let mut dropped = 0;
    let mut blocks = Vec::new();
    {
        let iter = device.rx_blocks().expect("rx_blocks");
        for transfer in iter.take(count) {
            dropped += transfer.dropped_samples;
            blocks.push(transfer.samples);
        }
    }
    assert!(!device.is_streaming(), "stream still live after drop");
    (blocks, dropped)
}

#[test]
#[ignore = "requires Airspy hardware"]
fn streams_every_sample_type() {
    let mut device = Device::open().expect("open");
    device.set_samplerate(0).expect("rate");
    device.set_freq(TEST_FREQ_HZ).expect("freq");

    for sample_type in [
        SampleType::Float32Iq,
        SampleType::Float32Real,
        SampleType::Int16Iq,
        SampleType::Int16Real,
        SampleType::Uint16Real,
        SampleType::Raw,
    ] {
        device.set_sample_type(sample_type).expect("set type");
        let (blocks, _) = capture_blocks(&mut device, SHORT_STREAM_BLOCKS);
        assert_eq!(blocks.len(), SHORT_STREAM_BLOCKS, "{sample_type:?}");
        for block in &blocks {
            let (len, finite) = match block {
                SampleBlock::Float32(s) => (s.len(), s.iter().all(|v| v.is_finite())),
                SampleBlock::Int16(s) => (s.len(), true),
                SampleBlock::Uint16(s) => (s.len(), true),
                SampleBlock::Raw(s) => (s.len(), true),
            };
            assert!(len > 0, "{sample_type:?}: empty block");
            assert!(finite, "{sample_type:?}: non-finite samples");
        }
        println!("{sample_type:?}: {} blocks OK", blocks.len());
    }
}

#[test]
#[ignore = "requires Airspy hardware"]
fn packed_raw_capture_streams() {
    let mut device = Device::open().expect("open");
    device.set_samplerate(0).expect("rate");
    device.set_freq(TEST_FREQ_HZ).expect("freq");
    device.set_sample_type(SampleType::Raw).expect("type");
    device.set_packing(true).expect("packing on");
    let (blocks, _) = capture_blocks(&mut device, SHORT_STREAM_BLOCKS);
    assert_eq!(blocks.len(), SHORT_STREAM_BLOCKS);
    for block in &blocks {
        let SampleBlock::Raw(bytes) = block else {
            unreachable!("expected raw block, got {block:?}");
        };
        assert!(!bytes.is_empty());
    }
    device.set_packing(false).expect("packing off again");
}

#[test]
#[ignore = "requires Airspy hardware"]
fn sustained_capture_has_no_drops() {
    const CAPTURE_SECONDS: u64 = 3;

    let mut device = Device::open().expect("open");
    device.set_samplerate(0).expect("rate (index 0 = fastest)");
    device.set_freq(TEST_FREQ_HZ).expect("freq");
    device
        .set_sample_type(SampleType::Int16Iq)
        .expect("int16 iq");

    let rates = device.samplerates();
    let nominal = rates.first().copied().expect("a rate");

    let mut dropped = 0u64;
    let mut frames = 0u64;
    let started = Instant::now();
    {
        let iter = device.rx_blocks().expect("rx_blocks");
        for transfer in iter {
            dropped += transfer.dropped_samples;
            if let SampleBlock::Int16(samples) = &transfer.samples {
                frames += samples.len() as u64 / 2;
            }
            if started.elapsed() >= Duration::from_secs(CAPTURE_SECONDS) {
                break;
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    // Frame counts over a few seconds sit far below 2^52.
    #[allow(clippy::cast_precision_loss)]
    let rate = frames as f64 / elapsed;
    println!(
        "sustained: {frames} IQ frames in {elapsed:.2}s = {:.3} MSPS (nominal {:.1}), dropped {dropped}",
        rate / 1e6,
        f64::from(nominal) / 1e6
    );
    assert_eq!(dropped, 0, "dropped samples during sustained capture");
    // Within 10% of nominal proves the pipeline keeps up.
    assert!(
        rate > f64::from(nominal) * 0.9,
        "throughput {rate} too far below nominal {nominal}"
    );
}

#[test]
#[ignore = "requires Airspy hardware"]
fn stream_restarts_cleanly() {
    let mut device = Device::open().expect("open");
    device.set_samplerate(0).expect("rate");
    device.set_freq(TEST_FREQ_HZ).expect("freq");
    device.set_sample_type(SampleType::Float32Iq).expect("type");
    for round in 0..3 {
        let (blocks, _) = capture_blocks(&mut device, 2);
        assert_eq!(blocks.len(), 2, "round {round}");
    }
}
