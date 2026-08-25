//! Port of `airspy_rx.c`: capture samples to a file (or stdout) with
//! sample-type, rate, frequency, gain, packing, bias-T, and
//! sample-count controls, plus SDR#-compatible WAV output. Console
//! output and the runtime sequence track the C tool so the two can be
//! diffed; deviations from upstream bugs are documented inline.

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use airspy_tools::rx::{
    FD_BUFFER_SIZE, RateTracker, apply_byte_budget, extend_sample_bytes, frame_count,
    resolve_display_rate,
};
use airspy_tools::rx_cli::{Args, Config, build_config, print_verbose, usage};
use airspy_tools::wav::{WAV_MAX_DATA_BYTES, wav_header_finalized, wav_header_placeholder};
use clap::Parser;
use libairspy_rs::commands::SampleType;
use libairspy_rs::stream::Transfer;
use libairspy_rs::{Device, Error};

/// The `0.000001f` Hz→MSPS factor in the C tool's verbose
/// `sample_rate -a` print (`airspy_rx.c` `main()`).
const HZ_TO_MSPS: f64 = 0.000_001;

/// One second — the C main loop's `sleep(1)` cadence.
const LOOP_SLEEP: std::time::Duration = std::time::Duration::from_secs(1);

/// `do_exit` in `airspy_rx.c`, flipped by the signal handler.
static DO_EXIT: AtomicBool = AtomicBool::new(false);

/// `context() failed: name (code)` — the C error-print pattern.
fn print_error(context: &str, err: &Error) {
    eprintln!("{context} failed: {} ({})", err.name(), err.code());
}

/// The callback-side state shared with the main loop — C's globals
/// (`fd`, the rate bookkeeping, `bytes_to_xfer`).
struct RxShared {
    writer: Option<Box<dyn Write + Send>>,
    tracker: RateTracker,
    /// Remaining byte budget: the `-n` limit, the WAV data cap, or
    /// both (minimum).
    remaining: Option<u64>,
    /// Reused per-block byte buffer.
    buf: Vec<u8>,
    /// Set when an output write failed — the capture is truncated and
    /// the exit status must be nonzero.
    write_failed: bool,
}

/// Lock the shared state, recovering from a poisoned mutex (a panic
/// on the consumer thread already stops the stream).
fn lock_shared(shared: &Mutex<RxShared>) -> MutexGuard<'_, RxShared> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// `rx_callback` in `airspy_rx.c`: track the rate, honor the `-n` byte
/// budget, and write the block; returning `false` is C's `-1` stop.
fn on_transfer(
    shared: &Mutex<RxShared>,
    started: Instant,
    sample_type: SampleType,
    packing: bool,
    transfer: &Transfer<'_>,
) -> bool {
    let mut state = lock_shared(shared);
    let state = &mut *state;
    let frames = frame_count(&transfer.samples, sample_type, packing);
    state
        .tracker
        .on_block(frames, started.elapsed().as_secs_f64());

    state.buf.clear();
    extend_sample_bytes(&mut state.buf, &transfer.samples);
    let bytes_to_write = apply_byte_budget(&mut state.remaining, state.buf.len());

    let Some(writer) = state.writer.as_mut() else {
        return false;
    };
    if writer.write_all(&state.buf[..bytes_to_write]).is_err() {
        // Recorded so the exit status reflects the truncated capture
        // (C's write-error stop keeps returning EXIT_SUCCESS).
        state.write_failed = true;
        return false;
    }
    // C: stop once the byte budget is exhausted.
    state.remaining != Some(0)
}

fn main() {
    let args = Args::parse();
    let date_time = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let config = match build_config(&args, &date_time) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            usage();
            std::process::exit(1);
        }
    };
    if config.receive_wav {
        eprintln!("Receive wav file: {}", config.path);
    }
    if config.verbose {
        print_verbose(&config);
    }
    std::process::exit(run(&config));
}

/// The verbose `sample_rate -a` line from C `main()`.
fn print_samplerate(config: &Config, wav_sample_per_sec: u32) {
    eprintln!(
        "sample_rate -a {} ({:.6} MSPS {})",
        config.sample_rate_val,
        f64::from(wav_sample_per_sec) * HZ_TO_MSPS,
        if config.wav_params.channels == 1 {
            "Real"
        } else {
            "IQ"
        }
    );
}

/// The `airspy_board_partid_serialno_read` call and serial print
/// from C `main()`.
fn print_device_serial(device: &Device) -> Result<(), ()> {
    match device.partid_serialno() {
        Ok(partid_serialno) => {
            eprintln!(
                "Device Serial Number: 0x{:08X}{:08X}",
                partid_serialno.serial_no[2], partid_serialno.serial_no[3]
            );
            Ok(())
        }
        Err(err) => {
            print_error("airspy_board_partid_serialno_read()", &err);
            Err(())
        }
    }
}

/// Open the device and apply the pre-stream configuration in C
/// `main()`'s exact order: sample type, sample-rate resolution and
/// set, serial-number print, packing, bias-T. Returns the device and
/// the resolved WAV/display rate; every failure prints C's message.
fn open_configured_device(config: &Config) -> Result<(Device, u32), ()> {
    let open_result = match config.serial_number {
        Some(serial) => (Device::open_serial(serial), "airspy_open_sn()"),
        None => (Device::open(), "airspy_open()"),
    };
    let mut device = match open_result {
        (Ok(device), _) => device,
        (Err(err), context) => {
            print_error(context, &err);
            return Err(());
        }
    };

    if let Err(err) = device.set_sample_type(config.sample_type) {
        print_error("airspy_set_sample_type()", &err);
        return Err(());
    }

    // C: airspy_get_samplerates count + table read, then the index-
    // or-literal resolution for the WAV/display rate.
    let rates = device.samplerates();
    let Some(wav_sample_per_sec) = resolve_display_rate(config.sample_rate_val, &rates) else {
        eprintln!("argument error: unsupported sample rate");
        return Err(());
    };

    if let Err(err) = device.set_samplerate(config.sample_rate_val) {
        print_error("airspy_set_samplerate()", &err);
        return Err(());
    }
    if config.verbose {
        print_samplerate(config, wav_sample_per_sec);
    }

    print_device_serial(&device)?;

    if config.call_set_packing
        && let Err(err) = device.set_packing(config.packing)
    {
        print_error("airspy_set_packing()", &err);
        return Err(());
    }

    if let Err(err) = device.set_rf_bias(config.biast) {
        print_error("airspy_set_rf_bias()", &err);
        return Err(());
    }

    Ok((device, wav_sample_per_sec))
}

/// Create the capture output (C's fopen + `setvbuf`; `-` is stdout).
fn open_output(config: &Config) -> Result<Box<dyn Write + Send>, ()> {
    if config.path == "-" {
        return Ok(Box::new(std::io::BufWriter::with_capacity(
            FD_BUFFER_SIZE,
            std::io::stdout(),
        )));
    }
    if let Ok(file) = std::fs::File::create(&config.path) {
        Ok(Box::new(std::io::BufWriter::with_capacity(
            FD_BUFFER_SIZE,
            file,
        )))
    } else {
        eprintln!("Failed to open file: {}", config.path);
        Err(())
    }
}

/// The gain selection before `airspy_start_rx` — C prints failures
/// but does not abort.
#[allow(clippy::cast_possible_truncation)]
fn apply_gains(device: &Device, config: &Config) {
    if config.linearity_gain.is_none() && config.sensitivity_gain.is_none() {
        if let Err(err) = device.set_vga_gain(config.vga_gain as u8) {
            print_error("airspy_set_vga_gain()", &err);
        }
        if let Err(err) = device.set_mixer_gain(config.mixer_gain as u8) {
            print_error("airspy_set_mixer_gain()", &err);
        }
        if let Err(err) = device.set_lna_gain(config.lna_gain as u8) {
            print_error("airspy_set_lna_gain()", &err);
        }
    } else {
        if let Some(gain) = config.linearity_gain
            && let Err(err) = device.set_linearity_gain(gain as u8)
        {
            print_error("airspy_set_linearity_gain()", &err);
        }
        if let Some(gain) = config.sensitivity_gain
            && let Err(err) = device.set_sensitivity_gain(gain as u8)
        {
            print_error("airspy_set_sensitivity_gain()", &err);
        }
    }
}

/// The 1 Hz status loop from C `main()`: print the smoothed rate,
/// and — exactly as C does — flip `do_exit` when the `-n` byte budget
/// empties (C then reports "User cancel, exiting...", the same as a
/// signal stop).
fn stream_loop(device: &Device, shared: &Mutex<RxShared>) {
    while device.is_streaming() && !DO_EXIT.load(Ordering::SeqCst) {
        let (average_rate, exhausted) = {
            let state = lock_shared(shared);
            (state.tracker.average_rate, state.remaining == Some(0))
        };
        eprintln!(
            "Streaming at {:>5} MSPS",
            format!("{:.3}", average_rate * 1e-6)
        );
        if exhausted {
            // C: do_exit = true when the -n budget empties (the WAV
            // size cap uses the same mechanism).
            DO_EXIT.store(true, Ordering::SeqCst);
        } else {
            std::thread::sleep(LOOP_SLEEP);
        }
    }
}

/// The exit summary from C `main()`: cancel/exit message, total time
/// from the first packet, and the windowed average speed.
#[allow(clippy::cast_precision_loss)]
fn print_summary(config: &Config, shared: &Mutex<RxShared>, started: Instant) {
    if DO_EXIT.load(Ordering::SeqCst) {
        eprintln!("\nUser cancel, exiting...");
    } else {
        eprintln!("\nExiting...");
    }

    let (total_time, global_average_rate, rate_samples) = {
        let state = lock_shared(shared);
        (
            started.elapsed().as_secs_f64() - state.tracker.t_start.unwrap_or(0.0),
            state.tracker.global_average_rate,
            state.tracker.rate_samples,
        )
    };
    eprintln!("Total time: {total_time:5.4} s");
    if rate_samples > 0 {
        eprintln!(
            "Average speed {:2.4} MSPS {}",
            global_average_rate * 1e-6 / rate_samples as f32,
            if config.wav_params.channels == 2 {
                "IQ"
            } else {
                "Real"
            }
        );
    }
}

/// C writes the placeholder header before streaming (fwrite,
/// unchecked there; a failure here cannot produce a valid capture).
fn write_wav_placeholder(config: &Config, shared: &Mutex<RxShared>) -> Result<(), ()> {
    let mut state = lock_shared(shared);
    if let Some(writer) = state.writer.as_mut()
        && writer.write_all(&wav_header_placeholder()).is_err()
    {
        eprintln!("Failed to open file: {}", config.path);
        return Err(());
    }
    Ok(())
}

/// C traps INT/ILL/FPE/SEGV/TERM/ABRT; the ctrlc termination feature
/// covers the catchable INT/TERM (plus HUP), so a SIGTERM still
/// finalizes the WAV header.
fn install_signal_handler() {
    let handler = ctrlc::set_handler(|| {
        eprintln!("Caught signal, exiting...");
        DO_EXIT.store(true, Ordering::SeqCst);
    });
    if handler.is_err() {
        eprintln!("Failed to install signal handler");
    }
}

/// Failure cleanup once the output (and possibly the stream) exists:
/// stop, drop the device, and finalize so a `-w` capture keeps a
/// valid header. Deviation: C exits from these paths leaving the
/// placeholder header unwritten.
fn abort_stream(
    mut device: Device,
    config: &Config,
    shared: &Mutex<RxShared>,
    wav_sample_per_sec: u32,
) -> i32 {
    if let Err(err) = device.stop_rx() {
        print_error("airspy_stop_rx()", &err);
    }
    drop(device);
    finalize_output(config, shared, wav_sample_per_sec);
    1
}

/// The device-facing half of C `main()`: open, configure, stream, and
/// report, in C's exact order.
#[allow(clippy::cast_precision_loss)]
fn run(config: &Config) -> i32 {
    let Ok((mut device, wav_sample_per_sec)) = open_configured_device(config) else {
        return 1;
    };
    let Ok(writer) = open_output(config) else {
        return 1;
    };

    // The -n byte budget, additionally capped at the RIFF size limit
    // for WAV output so the finalized header stays representable
    // (deviation: C wraps its uint32_t sizes past 4 GiB).
    let limit = config.limit_num_samples.then_some(config.bytes_to_xfer);
    let remaining = if config.receive_wav {
        Some(limit.map_or(WAV_MAX_DATA_BYTES, |b| b.min(WAV_MAX_DATA_BYTES)))
    } else {
        limit
    };
    let shared = Arc::new(Mutex::new(RxShared {
        writer: Some(writer),
        tracker: RateTracker::new(wav_sample_per_sec as f32),
        remaining,
        buf: Vec::new(),
        write_failed: false,
    }));

    if config.receive_wav && write_wav_placeholder(config, &shared).is_err() {
        return 1;
    }
    install_signal_handler();
    apply_gains(&device, config);

    let started = Instant::now();
    let callback_shared = Arc::clone(&shared);
    let (sample_type, packing) = (config.sample_type, config.packing);
    if let Err(err) = device.start_rx(move |transfer| {
        on_transfer(&callback_shared, started, sample_type, packing, &transfer)
    }) {
        print_error("airspy_start_rx()", &err);
        return abort_stream(device, config, &shared, wav_sample_per_sec);
    }

    // C sets the frequency after starting the stream. On failure C
    // exits leaving the WAV placeholder header unwritten (an unusable
    // file); deviation: stop the stream and finalize the output so
    // whatever was captured stays readable.
    if let Err(err) = device.set_freq(config.freq_hz) {
        print_error("airspy_set_freq()", &err);
        return abort_stream(device, config, &shared, wav_sample_per_sec);
    }

    eprintln!("Stop with Ctrl-C");
    std::thread::sleep(LOOP_SLEEP);
    stream_loop(&device, &shared);
    print_summary(config, &shared, started);

    if let Err(err) = device.stop_rx() {
        print_error("airspy_stop_rx()", &err);
    }
    drop(device);

    let output_ok = finalize_output(config, &shared, wav_sample_per_sec);
    eprintln!("done");
    // Deviation: C returns EXIT_SUCCESS even for truncated or
    // unfinalized captures; a failed output surfaces as status 1.
    i32::from(!output_ok)
}

/// The end-of-main file handling: flush and close the stream, then
/// for WAV captures rewrite the header with the real sizes (C's
/// `ftell` + rewind + `fwrite`).
fn finalize_output(config: &Config, shared: &Mutex<RxShared>, wav_sample_per_sec: u32) -> bool {
    let (writer, write_failed) = {
        let mut state = lock_shared(shared);
        (state.writer.take(), state.write_failed)
    };
    let mut ok = !write_failed;
    if write_failed {
        eprintln!("Failed to write file: {}", config.path);
    }
    if let Some(mut writer) = writer
        && writer.flush().is_err()
    {
        eprintln!("Failed to write file: {}", config.path);
        ok = false;
    }

    if config.receive_wav
        && let Err(message) = rewrite_wav_header(config, wav_sample_per_sec)
    {
        eprintln!("{message}");
        ok = false;
    }
    ok
}

/// Reopen the finished capture and overwrite the 44-byte header.
/// The streaming budget keeps `-w` files inside the RIFF limit, so
/// an unrepresentable size here (deviation: C wrapped its `uint32_t`
/// `ftell` position and wrote corrupt fields) can only mean outside
/// interference — refuse to finalize rather than truncate.
fn rewrite_wav_header(config: &Config, wav_sample_per_sec: u32) -> Result<(), String> {
    let fail = || format!("Failed to write file: {}", config.path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&config.path)
        .map_err(|_| fail())?;
    let len = file.metadata().map_err(|_| fail())?.len();
    let Ok(file_pos) = u32::try_from(len) else {
        return Err(format!(
            "{} exceeds the 4 GiB WAV size limit; header left unfinalized",
            config.path
        ));
    };
    let header = wav_header_finalized(file_pos, &config.wav_params, wav_sample_per_sec);
    file.seek(SeekFrom::Start(0)).map_err(|_| fail())?;
    file.write_all(&header).map_err(|_| fail())?;
    Ok(())
}
