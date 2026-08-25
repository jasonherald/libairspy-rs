//! Port of `airspy_rx.c`: capture samples to a file (or stdout) with
//! sample-type, rate, frequency, gain, packing, bias-T, and
//! sample-count controls, plus SDR#-compatible WAV output. Console
//! output and the runtime sequence track the C tool so the two can be
//! diffed; deviations from upstream bugs are documented inline.

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use airspy_tools::parse_u64;
use airspy_tools::rx::{
    BIAST_MAX, DEFAULT_FREQ_HZ, DEFAULT_LNA_GAIN, DEFAULT_MIXER_GAIN, DEFAULT_VGA_IF_GAIN,
    FD_BUFFER_SIZE, FREQ_HZ_MAX, FREQ_HZ_MIN, FREQ_ONE_MHZ, LINEARITY_GAIN_MAX, LNA_GAIN_MAX,
    MIXER_GAIN_MAX, RateTracker, SAMPLES_TO_XFER_MAX, SENSITIVITY_GAIN_MAX, VGA_GAIN_MAX,
    WavParams, bytes_to_xfer, extend_sample_bytes, frame_count, parse_freq_mhz, parse_u32,
    resolve_display_rate, wav_filename, wav_header_finalized, wav_header_placeholder,
};
use clap::Parser;
use libairspy_rs::commands::SampleType;
use libairspy_rs::stream::Transfer;
use libairspy_rs::{Device, Error};

/// `AIRSPY_RX_VERSION` in `airspy_rx.c`.
const AIRSPY_RX_VERSION: &str = "1.0.5 23 April 2016";

/// One second — the C main loop's `sleep(1)` cadence.
const LOOP_SLEEP: std::time::Duration = std::time::Duration::from_secs(1);

/// `do_exit` in `airspy_rx.c`, flipped by the signal handler.
static DO_EXIT: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(
    name = "airspy_rx",
    about = "Receive Airspy samples into a file",
    disable_help_flag = true
)]
struct Args {
    /// Receive data into file
    #[arg(short = 'r', value_name = "filename")]
    receive_file: Option<String>,

    /// Receive data into file with WAV header and automatic name
    #[arg(short = 'w')]
    receive_wav: bool,

    /// Open device with specified 64bits serial number
    #[arg(short = 's', value_name = "serial_number_64bits", value_parser = parse_u64)]
    serial_number: Option<u64>,

    /// Set packing for samples: 1=enabled(12bits packed), 0=disabled
    #[arg(short = 'p', value_name = "packing", value_parser = parse_u32)]
    packing: Option<u32>,

    /// Set frequency in MHz
    #[arg(short = 'f', value_name = "frequency_MHz")]
    freq_mhz: Option<String>,

    /// Set sample rate (index or Hz)
    #[arg(short = 'a', value_name = "sample_rate", value_parser = parse_u32)]
    sample_rate: Option<u32>,

    /// Set sample type: 0=`FLOAT32_IQ`, 1=`FLOAT32_REAL`, 2=`INT16_IQ`,
    /// 3=`INT16_REAL`, 4=`U16_REAL`, 5=RAW
    #[arg(short = 't', value_name = "sample_type", value_parser = parse_u32)]
    sample_type: Option<u32>,

    /// Set Bias Tee: 1=enabled, 0=disabled
    #[arg(short = 'b', value_name = "biast", value_parser = parse_u32)]
    biast: Option<u32>,

    /// Set VGA/IF gain, 0-15
    #[arg(short = 'v', value_name = "vga_gain", value_parser = parse_u32)]
    vga_gain: Option<u32>,

    /// Set Mixer gain, 0-15
    #[arg(short = 'm', value_name = "mixer_gain", value_parser = parse_u32)]
    mixer_gain: Option<u32>,

    /// Set LNA gain, 0-14
    #[arg(short = 'l', value_name = "lna_gain", value_parser = parse_u32)]
    lna_gain: Option<u32>,

    /// Set linearity simplified gain, 0-21
    #[arg(short = 'g', value_name = "linearity_gain", value_parser = parse_u32)]
    linearity_gain: Option<u32>,

    /// Set sensitivity simplified gain, 0-21 (C's -h; use --help for
    /// this text)
    #[arg(short = 'h', value_name = "sensitivity_gain", value_parser = parse_u32)]
    sensitivity_gain: Option<u32>,

    /// Number of samples to transfer (default is unlimited)
    #[arg(short = 'n', value_name = "num_samples", value_parser = parse_u64)]
    num_samples: Option<u64>,

    /// Verbose mode
    #[arg(short = 'd')]
    verbose: bool,

    /// Print help
    #[arg(long, action = clap::ArgAction::Help)]
    help: Option<bool>,
}

/// The `usage()` text from `airspy_rx.c`, printed after argument
/// errors.
fn usage() {
    eprintln!("airspy_rx v{AIRSPY_RX_VERSION}");
    eprintln!("Usage:");
    eprintln!("-r <filename>: Receive data into file");
    eprintln!("-w Receive data into file with WAV header and automatic name");
    eprintln!(" This is for SDR# compatibility and may not work with other software");
    eprintln!("[-s serial_number_64bits]: Open device with specified 64bits serial number");
    eprintln!("[-p packing]: Set packing for samples, ");
    eprintln!(" 1=enabled(12bits packed), 0=disabled(default 16bits not packed)");
    eprintln!(
        "[-f frequency_MHz]: Set frequency in MHz between [{}, {}] (default {}MHz)",
        FREQ_HZ_MIN / FREQ_ONE_MHZ,
        FREQ_HZ_MAX / FREQ_ONE_MHZ,
        DEFAULT_FREQ_HZ / FREQ_ONE_MHZ
    );
    eprintln!("[-a sample_rate]: Set sample rate");
    eprintln!("[-t sample_type]: Set sample type, ");
    eprintln!(
        " 0=FLOAT32_IQ, 1=FLOAT32_REAL, 2=INT16_IQ(default), 3=INT16_REAL, 4=U16_REAL, 5=RAW"
    );
    eprintln!("[-b biast]: Set Bias Tee, 1=enabled, 0=disabled(default)");
    eprintln!("[-v vga_gain]: Set VGA/IF gain, 0-{VGA_GAIN_MAX} (default {DEFAULT_VGA_IF_GAIN})");
    eprintln!("[-m mixer_gain]: Set Mixer gain, 0-{MIXER_GAIN_MAX} (default {DEFAULT_MIXER_GAIN})");
    eprintln!("[-l lna_gain]: Set LNA gain, 0-{LNA_GAIN_MAX} (default {DEFAULT_LNA_GAIN})");
    eprintln!("[-g linearity_gain]: Set linearity simplified gain, 0-{LINEARITY_GAIN_MAX}");
    eprintln!("[-h sensivity_gain]: Set sensitivity simplified gain, 0-{SENSITIVITY_GAIN_MAX}");
    eprintln!("[-n num_samples]: Number of samples to transfer (default is unlimited)");
    eprintln!("[-d]: Verbose mode");
}

/// The validated capture parameters, C's checked globals after the
/// argument section of `main()`.
// The bools mirror the C tool's independent flag globals.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct Config {
    path: String,
    receive_wav: bool,
    serial_number: Option<u64>,
    packing: bool,
    call_set_packing: bool,
    freq_hz: u32,
    sample_rate_val: u32,
    sample_type: SampleType,
    wav_params: WavParams,
    biast: bool,
    vga_gain: u32,
    mixer_gain: u32,
    lna_gain: u32,
    linearity_gain: Option<u32>,
    sensitivity_gain: Option<u32>,
    limit_num_samples: bool,
    samples_to_xfer: u64,
    bytes_to_xfer: u64,
    verbose: bool,
}

/// The `-t` value → sample type mapping from the C switch (`None` is
/// the out-of-range marker checked later).
fn sample_type_from_u32(value: u32) -> Option<SampleType> {
    match value {
        0 => Some(SampleType::Float32Iq),
        1 => Some(SampleType::Float32Real),
        2 => Some(SampleType::Int16Iq),
        3 => Some(SampleType::Int16Real),
        4 => Some(SampleType::Uint16Real),
        5 => Some(SampleType::Raw),
        _ => None,
    }
}

/// Every post-getopt validation from `airspy_rx.c` `main()`, in C's order
/// and with C's stderr messages. `date_time` is the pre-formatted
/// `%Y%m%d_%H%M%S` local time for the `-w` automatic filename.
///
/// Deviation: C's `-b` case sets the *`serial_number`* flag instead of
/// a bias-T flag (a copy-paste bug that makes a bare `-b 1` open
/// serial number 0); here `-b` only controls the bias tee.
#[allow(clippy::too_many_lines)]
fn build_config(args: &Args, date_time: &str) -> Result<Config, String> {
    let sample_type = match args.sample_type {
        None => Some(SampleType::Int16Iq),
        Some(v) => sample_type_from_u32(v),
    };
    // The WAV parameters fall back to the int16 defaults while the
    // sample type is invalid, exactly as C's globals do; the
    // sample_type range check below fires before they are used.
    let wav_params = WavParams::for_sample_type(sample_type.unwrap_or(SampleType::Int16Iq));

    let samples_to_xfer = args.num_samples.unwrap_or(0);
    let effective_type = sample_type.unwrap_or(SampleType::Int16Iq);
    let packing = args.packing == Some(1);
    let bytes = bytes_to_xfer(samples_to_xfer, &wav_params, effective_type, packing);

    if samples_to_xfer >= SAMPLES_TO_XFER_MAX {
        return Err(format!(
            "argument error: num_samples must be less than {SAMPLES_TO_XFER_MAX}/{}Mio",
            SAMPLES_TO_XFER_MAX / u64::from(FREQ_ONE_MHZ)
        ));
    }

    let freq_hz = match &args.freq_mhz {
        Some(s) => {
            let hz = parse_freq_mhz(s);
            if !(FREQ_HZ_MIN..FREQ_HZ_MAX).contains(&hz) {
                return Err(format!(
                    "argument error: frequency_MHz={:.6} MHz and shall be between [{}, {}[ MHz",
                    f64::from(hz) / f64::from(FREQ_ONE_MHZ),
                    FREQ_HZ_MIN / FREQ_ONE_MHZ,
                    FREQ_HZ_MAX / FREQ_ONE_MHZ
                ));
            }
            hz
        }
        None => DEFAULT_FREQ_HZ,
    };

    let path = if args.receive_wav {
        if sample_type == Some(SampleType::Raw) {
            return Err("The RAW sampling mode is not compatible with Wave files".into());
        }
        wav_filename(date_time, freq_hz)
    } else {
        match &args.receive_file {
            Some(p) => p.clone(),
            None => {
                return Err(
                    "error: you shall specify at least -r <with filename> or -w option".into(),
                );
            }
        }
    };

    if let Some(p) = args.packing
        && p > 1
    {
        return Err("argument error: packing out of range".into());
    }
    let Some(sample_type) = sample_type else {
        return Err("argument error: sample_type out of range".into());
    };
    let biast_val = args.biast.unwrap_or(0);
    if biast_val > BIAST_MAX {
        return Err("argument error: biast_val out of range".into());
    }
    let vga_gain = args.vga_gain.unwrap_or(DEFAULT_VGA_IF_GAIN);
    if vga_gain > VGA_GAIN_MAX {
        return Err("argument error: vga_gain out of range".into());
    }
    let mixer_gain = args.mixer_gain.unwrap_or(DEFAULT_MIXER_GAIN);
    if mixer_gain > MIXER_GAIN_MAX {
        return Err("argument error: mixer_gain out of range".into());
    }
    let lna_gain = args.lna_gain.unwrap_or(DEFAULT_LNA_GAIN);
    if lna_gain > LNA_GAIN_MAX {
        return Err("argument error: lna_gain out of range".into());
    }
    if args.linearity_gain.unwrap_or(0) > LINEARITY_GAIN_MAX {
        return Err("argument error: linearity_gain out of range".into());
    }
    if args.sensitivity_gain.unwrap_or(0) > SENSITIVITY_GAIN_MAX {
        return Err("argument error: sensitivity_gain out of range".into());
    }
    if args.linearity_gain.is_some() && args.sensitivity_gain.is_some() {
        return Err(
            "argument error: linearity_gain and sensitivity_gain are both set (choose only one option)"
                .into(),
        );
    }

    Ok(Config {
        path,
        receive_wav: args.receive_wav,
        serial_number: args.serial_number,
        packing,
        call_set_packing: args.packing.is_some(),
        freq_hz,
        sample_rate_val: args.sample_rate.unwrap_or(0),
        sample_type,
        wav_params,
        biast: biast_val == 1,
        vga_gain,
        mixer_gain,
        lna_gain,
        linearity_gain: args.linearity_gain,
        sensitivity_gain: args.sensitivity_gain,
        limit_num_samples: args.num_samples.is_some(),
        samples_to_xfer,
        bytes_to_xfer: bytes,
        verbose: args.verbose,
    })
}

/// The `if (verbose)` argument dump in `airspy_rx.c` `main()`.
fn print_verbose(config: &Config) {
    eprintln!("airspy_rx v{AIRSPY_RX_VERSION}");
    if let Some(serial) = config.serial_number {
        eprintln!(
            "serial_number_64bits -s 0x{:08X}{:08X}",
            (serial >> 32) as u32,
            (serial & 0xFFFF_FFFF) as u32
        );
    }
    eprintln!("packing -p {}", u32::from(config.packing));
    eprintln!(
        "frequency_MHz -f {:.6}MHz ({}Hz)",
        f64::from(config.freq_hz) / f64::from(FREQ_ONE_MHZ),
        config.freq_hz
    );
    eprintln!("sample_type -t {}", config.sample_type as u8);
    eprintln!("biast -b {}", u32::from(config.biast));
    if config.linearity_gain.is_none() && config.sensitivity_gain.is_none() {
        eprintln!("vga_gain -v {}", config.vga_gain);
        eprintln!("mixer_gain -m {}", config.mixer_gain);
        eprintln!("lna_gain -l {}", config.lna_gain);
    } else {
        if let Some(g) = config.linearity_gain {
            eprintln!("linearity_gain -g {g}");
        }
        if let Some(g) = config.sensitivity_gain {
            eprintln!("sensitivity_gain -h {g}");
        }
    }
    if config.limit_num_samples {
        eprintln!(
            "num_samples -n {} ({}M)",
            config.samples_to_xfer,
            config.samples_to_xfer / u64::from(FREQ_ONE_MHZ)
        );
    }
}

/// `context() failed: name (code)` — the C error-print pattern.
fn print_error(context: &str, err: &Error) {
    eprintln!("{context} failed: {} ({})", err.name(), err.code());
}

/// The callback-side state shared with the main loop — C's globals
/// (`fd`, the rate bookkeeping, `bytes_to_xfer`).
struct RxShared {
    writer: Option<Box<dyn Write + Send>>,
    tracker: RateTracker,
    /// Remaining byte budget when `-n` was given.
    remaining: Option<u64>,
    /// Reused per-block byte buffer.
    buf: Vec<u8>,
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
    let mut bytes_to_write = state.buf.len();
    if let Some(remaining) = state.remaining.as_mut() {
        #[allow(clippy::cast_possible_truncation)]
        if bytes_to_write as u64 >= *remaining {
            bytes_to_write = *remaining as usize;
        }
        *remaining -= bytes_to_write as u64;
    }

    let Some(writer) = state.writer.as_mut() else {
        return false;
    };
    if writer.write_all(&state.buf[..bytes_to_write]).is_err() {
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

/// The device-facing half of C `main()`: open, configure in C's exact
/// order, stream, and report — every failure prints C's message.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn run(config: &Config) -> i32 {
    let open_result = match config.serial_number {
        Some(serial) => (Device::open_serial(serial), "airspy_open_sn()"),
        None => (Device::open(), "airspy_open()"),
    };
    let mut device = match open_result {
        (Ok(device), _) => device,
        (Err(err), context) => {
            print_error(context, &err);
            return 1;
        }
    };

    if let Err(err) = device.set_sample_type(config.sample_type) {
        print_error("airspy_set_sample_type()", &err);
        return 1;
    }

    // C: airspy_get_samplerates count + table read, then the index-
    // or-literal resolution for the WAV/display rate.
    let rates = device.samplerates();
    let Some(wav_sample_per_sec) = resolve_display_rate(config.sample_rate_val, &rates) else {
        eprintln!("argument error: unsupported sample rate");
        return 1;
    };

    if let Err(err) = device.set_samplerate(config.sample_rate_val) {
        print_error("airspy_set_samplerate()", &err);
        return 1;
    }
    if config.verbose {
        eprintln!(
            "sample_rate -a {} ({:.6} MSPS {})",
            config.sample_rate_val,
            f64::from(wav_sample_per_sec) * 0.000_001,
            if config.wav_params.channels == 1 {
                "Real"
            } else {
                "IQ"
            }
        );
    }

    match device.partid_serialno() {
        Ok(partid_serialno) => eprintln!(
            "Device Serial Number: 0x{:08X}{:08X}",
            partid_serialno.serial_no[2], partid_serialno.serial_no[3]
        ),
        Err(err) => {
            print_error("airspy_board_partid_serialno_read()", &err);
            return 1;
        }
    }

    if config.call_set_packing
        && let Err(err) = device.set_packing(config.packing)
    {
        print_error("airspy_set_packing()", &err);
        return 1;
    }

    if let Err(err) = device.set_rf_bias(config.biast) {
        print_error("airspy_set_rf_bias()", &err);
        return 1;
    }

    let writer: Box<dyn Write + Send> = if config.path == "-" {
        Box::new(std::io::BufWriter::with_capacity(
            FD_BUFFER_SIZE,
            std::io::stdout(),
        ))
    } else if let Ok(file) = std::fs::File::create(&config.path) {
        Box::new(std::io::BufWriter::with_capacity(FD_BUFFER_SIZE, file))
    } else {
        eprintln!("Failed to open file: {}", config.path);
        return 1;
    };

    let shared = Arc::new(Mutex::new(RxShared {
        writer: Some(writer),
        tracker: RateTracker::new(wav_sample_per_sec as f32),
        remaining: config.limit_num_samples.then_some(config.bytes_to_xfer),
        buf: Vec::new(),
    }));

    // C writes the placeholder header before streaming (fwrite,
    // unchecked there; a failure here cannot produce a valid capture).
    if config.receive_wav {
        let mut state = lock_shared(&shared);
        if let Some(writer) = state.writer.as_mut()
            && writer.write_all(&wav_header_placeholder()).is_err()
        {
            eprintln!("Failed to open file: {}", config.path);
            return 1;
        }
    }

    // C traps INT/ILL/FPE/SEGV/TERM/ABRT; only the catchable
    // INT/TERM pair maps to a safe Rust handler.
    let handler = ctrlc::set_handler(|| {
        eprintln!("Caught signal, exiting...");
        DO_EXIT.store(true, Ordering::SeqCst);
    });
    if handler.is_err() {
        eprintln!("Failed to install signal handler");
    }

    // C: gain failures print but do not abort.
    if config.linearity_gain.is_none() && config.sensitivity_gain.is_none() {
        #[allow(clippy::cast_possible_truncation)]
        {
            if let Err(err) = device.set_vga_gain(config.vga_gain as u8) {
                print_error("airspy_set_vga_gain()", &err);
            }
            if let Err(err) = device.set_mixer_gain(config.mixer_gain as u8) {
                print_error("airspy_set_mixer_gain()", &err);
            }
            if let Err(err) = device.set_lna_gain(config.lna_gain as u8) {
                print_error("airspy_set_lna_gain()", &err);
            }
        }
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
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

    let started = Instant::now();
    let callback_shared = Arc::clone(&shared);
    let (sample_type, packing) = (config.sample_type, config.packing);
    if let Err(err) = device.start_rx(move |transfer| {
        on_transfer(&callback_shared, started, sample_type, packing, &transfer)
    }) {
        print_error("airspy_start_rx()", &err);
        return 1;
    }

    // C sets the frequency after starting the stream.
    if let Err(err) = device.set_freq(config.freq_hz) {
        print_error("airspy_set_freq()", &err);
        return 1;
    }

    eprintln!("Stop with Ctrl-C");
    std::thread::sleep(LOOP_SLEEP);

    while device.is_streaming() && !DO_EXIT.load(Ordering::SeqCst) {
        let (average_rate, exhausted) = {
            let state = lock_shared(&shared);
            (state.tracker.average_rate, state.remaining == Some(0))
        };
        eprintln!(
            "Streaming at {:>5} MSPS",
            format!("{:.3}", average_rate * 1e-6)
        );
        if config.limit_num_samples && exhausted {
            DO_EXIT.store(true, Ordering::SeqCst);
        } else {
            std::thread::sleep(LOOP_SLEEP);
        }
    }

    if DO_EXIT.load(Ordering::SeqCst) {
        eprintln!("\nUser cancel, exiting...");
    } else {
        eprintln!("\nExiting...");
    }

    let (total_time, global_average_rate, rate_samples) = {
        let state = lock_shared(&shared);
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

    if let Err(err) = device.stop_rx() {
        print_error("airspy_stop_rx()", &err);
    }
    drop(device);

    finalize_output(config, &shared, wav_sample_per_sec);
    eprintln!("done");
    0
}

/// The end-of-main file handling: flush and close the stream, then
/// for WAV captures rewrite the header with the real sizes (C's
/// `ftell` + rewind + `fwrite`).
fn finalize_output(config: &Config, shared: &Mutex<RxShared>, wav_sample_per_sec: u32) {
    let writer = lock_shared(shared).writer.take();
    if let Some(mut writer) = writer
        && writer.flush().is_err()
    {
        eprintln!("Failed to write file: {}", config.path);
    }

    if config.receive_wav
        && let Err(message) = rewrite_wav_header(config, wav_sample_per_sec)
    {
        eprintln!("{message}");
    }
}

/// Reopen the finished capture and overwrite the 44-byte header. C
/// stores `ftell`'s position in a `uint32_t`, so files past 4 GiB wrap
/// the size fields (a WAV format limit); the truncating cast keeps
/// that behavior.
#[allow(clippy::cast_possible_truncation)]
fn rewrite_wav_header(config: &Config, wav_sample_per_sec: u32) -> Result<(), String> {
    let fail = || format!("Failed to write file: {}", config.path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&config.path)
        .map_err(|_| fail())?;
    let file_pos = file.metadata().map_err(|_| fail())?.len() as u32;
    let header = wav_header_finalized(file_pos, &config.wav_params, wav_sample_per_sec);
    file.seek(SeekFrom::Start(0)).map_err(|_| fail())?;
    file.write_all(&header).map_err(|_| fail())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("airspy_rx").chain(argv.iter().copied()))
            .expect("args parse")
    }

    #[test]
    fn args_mirror_c_flags() {
        let args = parse(&[
            "-r", "out.bin", "-s", "0x1234", "-p", "1", "-f", "100.5", "-a", "1", "-t", "0", "-b",
            "1", "-v", "10", "-m", "9", "-l", "8", "-n", "5000", "-d",
        ]);
        assert_eq!(args.receive_file.as_deref(), Some("out.bin"));
        assert_eq!(args.serial_number, Some(0x1234));
        assert_eq!(args.packing, Some(1));
        assert_eq!(args.freq_mhz.as_deref(), Some("100.5"));
        assert_eq!(args.sample_rate, Some(1));
        assert_eq!(args.sample_type, Some(0));
        assert_eq!(args.biast, Some(1));
        assert_eq!(
            (args.vga_gain, args.mixer_gain, args.lna_gain),
            (Some(10), Some(9), Some(8))
        );
        assert_eq!(args.num_samples, Some(5000));
        assert!(args.verbose);
    }

    #[test]
    fn dash_h_is_sensitivity_gain_not_help() {
        // C's -h is the sensitivity gain; help moved to --help.
        let args = parse(&["-r", "f", "-h", "12"]);
        assert_eq!(args.sensitivity_gain, Some(12));
    }

    #[test]
    fn config_defaults_match_c_globals() {
        let config = build_config(&parse(&["-r", "f"]), "20260101_000000").expect("config");
        assert_eq!(config.freq_hz, DEFAULT_FREQ_HZ);
        assert_eq!(config.sample_type, SampleType::Int16Iq);
        assert_eq!(config.sample_rate_val, 0);
        assert_eq!(
            (config.vga_gain, config.mixer_gain, config.lna_gain),
            (DEFAULT_VGA_IF_GAIN, DEFAULT_MIXER_GAIN, DEFAULT_LNA_GAIN)
        );
        assert!(!config.biast);
        assert!(!config.packing && !config.call_set_packing);
        assert!(!config.limit_num_samples);
    }

    #[test]
    fn validation_rejects_out_of_range_values_in_c_order() {
        let cases: &[(&[&str], &str)] = &[
            (&["-r", "f", "-f", "2000"], "frequency_MHz"),
            (&["-w", "-t", "5"], "RAW sampling mode"),
            (&[], "you shall specify at least"),
            (&["-r", "f", "-p", "2"], "packing out of range"),
            (&["-r", "f", "-t", "6"], "sample_type out of range"),
            (&["-r", "f", "-b", "2"], "biast_val out of range"),
            (&["-r", "f", "-v", "16"], "vga_gain out of range"),
            (&["-r", "f", "-m", "16"], "mixer_gain out of range"),
            (&["-r", "f", "-l", "15"], "lna_gain out of range"),
            (&["-r", "f", "-g", "22"], "linearity_gain out of range"),
            (&["-r", "f", "-h", "22"], "sensitivity_gain out of range"),
            (&["-r", "f", "-g", "1", "-h", "1"], "both set"),
        ];
        for (argv, expected) in cases {
            let err = build_config(&parse(argv), "20260101_000000").expect_err(expected);
            assert!(err.contains(expected), "{argv:?}: {err}");
        }
    }

    #[test]
    fn biast_does_not_imply_serial_open() {
        // Deviation: C's -b case sets the serial_number flag (a
        // copy-paste bug), silently switching a bare `-b 1` run to
        // airspy_open_sn(0). Here -b only drives the bias tee.
        let config =
            build_config(&parse(&["-r", "f", "-b", "1"]), "20260101_000000").expect("config");
        assert!(config.biast);
        assert_eq!(config.serial_number, None);
    }

    #[test]
    fn wav_mode_generates_c_filename() {
        let config = build_config(&parse(&["-w", "-f", "100"]), "20261225_101112").expect("config");
        assert!(config.receive_wav);
        assert_eq!(config.path, "AirSpy_20261225_101112Z_100000kHz_IQ.wav");
    }

    #[test]
    fn num_samples_limit_computes_byte_budget() {
        // int16 IQ: 16 bits * 2 channels / 8 = 4 bytes per sample.
        let config =
            build_config(&parse(&["-r", "f", "-n", "1000"]), "20260101_000000").expect("config");
        assert!(config.limit_num_samples);
        assert_eq!(config.bytes_to_xfer, 4000);
        let err = build_config(
            &parse(&["-r", "f", "-n", "0x8000000000000000"]),
            "20260101_000000",
        )
        .expect_err("max");
        assert!(err.contains("num_samples must be less than"));
    }
}
