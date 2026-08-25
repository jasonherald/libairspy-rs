//! The CLI layer of the `airspy_rx` binary, ported from
//! `airspy_rx.c`'s getopt loop, `usage()`, and the post-getopt
//! validation section of `main()` — split from the binary so the
//! whole argument surface is unit-testable.

use clap::Parser;
use libairspy_rs::commands::SampleType;

use crate::parse_u64;
use crate::rx::{
    BIAST_MAX, DEFAULT_FREQ_HZ, DEFAULT_LNA_GAIN, DEFAULT_MIXER_GAIN, DEFAULT_VGA_IF_GAIN,
    FREQ_HZ_MAX, FREQ_HZ_MIN, FREQ_ONE_MHZ, LINEARITY_GAIN_MAX, LNA_GAIN_MAX, MIXER_GAIN_MAX,
    SAMPLES_TO_XFER_MAX, SENSITIVITY_GAIN_MAX, VGA_GAIN_MAX, WavParams, bytes_to_xfer,
    parse_freq_mhz, parse_u32, wav_filename,
};

/// `AIRSPY_RX_VERSION` in `airspy_rx.c`.
pub const AIRSPY_RX_VERSION: &str = "1.0.5 23 April 2016";

/// The `airspy_rx` command line — one field per C getopt flag (the
/// doc comments double as `--help` text).
#[derive(Parser)]
#[command(
    name = "airspy_rx",
    about = "Receive Airspy samples into a file",
    disable_help_flag = true
)]
pub struct Args {
    /// Receive data into file
    #[arg(short = 'r', value_name = "filename")]
    pub receive_file: Option<String>,

    /// Receive data into file with WAV header and automatic name
    #[arg(short = 'w')]
    pub receive_wav: bool,

    /// Open device with specified 64bits serial number
    #[arg(short = 's', value_name = "serial_number_64bits", value_parser = parse_u64)]
    pub serial_number: Option<u64>,

    /// Set packing for samples: 1=enabled(12bits packed), 0=disabled
    #[arg(short = 'p', value_name = "packing", value_parser = parse_u32)]
    pub packing: Option<u32>,

    /// Set frequency in MHz
    #[arg(short = 'f', value_name = "frequency_MHz")]
    pub freq_mhz: Option<String>,

    /// Set sample rate (index or Hz)
    #[arg(short = 'a', value_name = "sample_rate", value_parser = parse_u32)]
    pub sample_rate: Option<u32>,

    /// Set sample type: 0=`FLOAT32_IQ`, 1=`FLOAT32_REAL`, 2=`INT16_IQ`,
    /// 3=`INT16_REAL`, 4=`U16_REAL`, 5=RAW
    #[arg(short = 't', value_name = "sample_type", value_parser = parse_u32)]
    pub sample_type: Option<u32>,

    /// Set Bias Tee: 1=enabled, 0=disabled
    #[arg(short = 'b', value_name = "biast", value_parser = parse_u32)]
    pub biast: Option<u32>,

    /// Set VGA/IF gain, 0-15
    #[arg(short = 'v', value_name = "vga_gain", value_parser = parse_u32)]
    pub vga_gain: Option<u32>,

    /// Set Mixer gain, 0-15
    #[arg(short = 'm', value_name = "mixer_gain", value_parser = parse_u32)]
    pub mixer_gain: Option<u32>,

    /// Set LNA gain, 0-14
    #[arg(short = 'l', value_name = "lna_gain", value_parser = parse_u32)]
    pub lna_gain: Option<u32>,

    /// Set linearity simplified gain, 0-21
    #[arg(short = 'g', value_name = "linearity_gain", value_parser = parse_u32)]
    pub linearity_gain: Option<u32>,

    /// Set sensitivity simplified gain, 0-21 (C's -h; use --help for
    /// this text)
    #[arg(short = 'h', value_name = "sensitivity_gain", value_parser = parse_u32)]
    pub sensitivity_gain: Option<u32>,

    /// Number of samples to transfer (default is unlimited)
    #[arg(short = 'n', value_name = "num_samples", value_parser = parse_u64)]
    pub num_samples: Option<u64>,

    /// Verbose mode
    #[arg(short = 'd')]
    pub verbose: bool,

    /// Print help
    #[arg(long, action = clap::ArgAction::Help)]
    pub help: Option<bool>,
}

/// The `usage()` text from `airspy_rx.c`, printed after argument
/// errors.
pub fn usage() {
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
pub struct Config {
    /// Output path (`-` is stdout; generated for `-w`).
    pub path: String,
    /// `-w`: WAV output with the automatic filename.
    pub receive_wav: bool,
    /// `-s`: open by 64-bit serial number.
    pub serial_number: Option<u64>,
    /// The `-p` value (12-bit packed transfers).
    pub packing: bool,
    /// Whether `-p` was given (C's `call_set_packing`).
    pub call_set_packing: bool,
    /// The tune frequency in Hz (validated or default).
    pub freq_hz: u32,
    /// The raw `-a` value (index or literal Hz; 0 default).
    pub sample_rate_val: u32,
    /// The validated `-t` selection.
    pub sample_type: SampleType,
    /// WAV format fields derived from the sample type.
    pub wav_params: WavParams,
    /// The `-b` value (bias tee).
    pub biast: bool,
    /// `-v` (validated, defaulted).
    pub vga_gain: u32,
    /// `-m` (validated, defaulted).
    pub mixer_gain: u32,
    /// `-l` (validated, defaulted).
    pub lna_gain: u32,
    /// `-g` if given.
    pub linearity_gain: Option<u32>,
    /// `-h` if given.
    pub sensitivity_gain: Option<u32>,
    /// Whether `-n` was given.
    pub limit_num_samples: bool,
    /// The `-n` sample count.
    pub samples_to_xfer: u64,
    /// The `-n` limit converted to output bytes.
    pub bytes_to_xfer: u64,
    /// `-d`.
    pub verbose: bool,
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

/// The `-f` handling from `airspy_rx.c` `main()`: parse with strtod
/// semantics, enforce `[FREQ_HZ_MIN, FREQ_HZ_MAX[`, default 900 MHz.
fn resolve_freq(args: &Args) -> Result<u32, String> {
    let Some(s) = &args.freq_mhz else {
        return Ok(DEFAULT_FREQ_HZ);
    };
    let hz = parse_freq_mhz(s);
    if (FREQ_HZ_MIN..FREQ_HZ_MAX).contains(&hz) {
        Ok(hz)
    } else {
        Err(format!(
            "argument error: frequency_MHz={:.6} MHz and shall be between [{}, {}[ MHz",
            f64::from(hz) / f64::from(FREQ_ONE_MHZ),
            FREQ_HZ_MIN / FREQ_ONE_MHZ,
            FREQ_HZ_MAX / FREQ_ONE_MHZ
        ))
    }
}

/// The output-path selection from `airspy_rx.c` `main()`: `-w` rejects
/// RAW and generates the dated filename; otherwise `-r` is required.
fn resolve_capture_path(
    args: &Args,
    sample_type: Option<SampleType>,
    freq_hz: u32,
    date_time: &str,
) -> Result<String, String> {
    if args.receive_wav {
        if sample_type == Some(SampleType::Raw) {
            return Err("The RAW sampling mode is not compatible with Wave files".into());
        }
        return Ok(wav_filename(date_time, freq_hz));
    }
    match &args.receive_file {
        Some(p) => Ok(p.clone()),
        None => Err("error: you shall specify at least -r <with filename> or -w option".into()),
    }
}

/// The bias-T and gain range checks from `airspy_rx.c` `main()`, in
/// C's order and with C's messages; returns the defaulted values.
fn validate_ranges(args: &Args) -> Result<(u32, u32, u32, u32), String> {
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
    Ok((biast_val, vga_gain, mixer_gain, lna_gain))
}

/// Every post-getopt validation from `airspy_rx.c` `main()`, in C's order
/// and with C's stderr messages. `date_time` is the pre-formatted
/// `%Y%m%d_%H%M%S` local time for the `-w` automatic filename.
///
/// Deviation: C's `-b` case sets the *`serial_number`* flag instead of
/// a bias-T flag (a copy-paste bug that makes a bare `-b 1` open
/// serial number 0); here `-b` only controls the bias tee.
pub fn build_config(args: &Args, date_time: &str) -> Result<Config, String> {
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

    let freq_hz = resolve_freq(args)?;
    let path = resolve_capture_path(args, sample_type, freq_hz, date_time)?;
    if let Some(p) = args.packing
        && p > 1
    {
        return Err("argument error: packing out of range".into());
    }
    let Some(sample_type) = sample_type else {
        return Err("argument error: sample_type out of range".into());
    };
    let (biast_val, vga_gain, mixer_gain, lna_gain) = validate_ranges(args)?;

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
pub fn print_verbose(config: &Config) {
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
