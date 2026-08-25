//! The `airspy_calibrate` clap command, in its own module so
//! Codacy's Lizard (which mis-lexes Rust char literals such as
//! `.short('r')`) has nothing meaningful to swallow after it.

/// The `airspy_calibrate` clap command — C's `getopt(argc, argv,
/// "rw:")`. This is the only tool without a serial option; kept
/// faithful. `--force` is the calibration-write confirmation
/// deviation, and `-w` parses strictly (deviation: C's `atoi`
/// silently turned garbage into 0 before writing it to flash).
pub fn calib_command() -> clap::Command {
    clap::Command::new("airspy_calibrate")
        .about("Read or write the Airspy calibration record")
        .disable_help_flag(true)
        .arg(
            clap::Arg::new("help")
                .long("help")
                .action(clap::ArgAction::Help)
                .help("Print help"),
        )
        .arg(
            clap::Arg::new("read")
                .short('r')
                .action(clap::ArgAction::SetTrue)
                .help("Read and display calibration data"),
        )
        .arg(
            clap::Arg::new("write")
                .short('w')
                .value_name("calibration in ppb")
                // getopt takes a negative optarg (-w -1500) natively.
                .allow_negative_numbers(true)
                .value_parser(clap::value_parser!(i32))
                .help("Erase and Write calibration in ppb"),
        )
        .arg(
            clap::Arg::new("force")
                .long("force")
                .action(clap::ArgAction::SetTrue)
                .help("confirm calibration writes (-w) — they erase the calibration sector"),
        )
}
