//! The `airspy_spiflash` clap command, split from `flash_cli` so
//! that module keeps its tests at the file bottom.
//!
//! NOTE: Codacy's Lizard mis-lexes Rust char literals (`.short('a')`
//! reads as a lifetime plus a dangling quote), swallowing every
//! following function into one measurement — this file holds only
//! the builders so the confusion has nothing meaningful to swallow.
//! The real fix belongs upstream in Lizard.

/// `parse_u32` for flash addresses and lengths: [`crate::parse_u64`]
/// semantics with a checked narrowing. Deviation: C's `strtoul`
/// truncates on LP64, so `-a 0x100000000` became address 0 — with a
/// forced write, an erase and a firmware write at the wrong address;
/// out-of-range values reject here.
fn parse_flash_u32(s: &str) -> Result<u32, crate::ParseU64Error> {
    u32::try_from(crate::parse_u64(s)?).map_err(|_| crate::ParseU64Error)
}

/// The `airspy_spiflash` clap command — C's `getopt_long(argc, argv,
/// "a:l:r:w:s:", long_options, ...)`. The C table's `--reset` entry
/// is omitted: it has no `case 't'` and no optstring letter, so it
/// only ever reached C's `opt error` path. `--force` is the
/// write-confirmation deviation (a `-w` erases and rewrites the
/// firmware flash).
pub fn flash_command() -> clap::Command {
    clap::Command::new("airspy_spiflash")
        .about("Read and write the Airspy SPI flash")
        .disable_help_flag(true)
        .args(range_args())
        .args(control_args())
}

/// The `-a`/`-l`/`-r`/`-w` transfer arguments from the C option table.
/// (Vec rather than an array: the `[T; N]` semicolon in a return
/// type confuses Lizard's function-boundary parsing.)
fn range_args() -> Vec<clap::Arg> {
    vec![
        clap::Arg::new("address")
            .short('a')
            .long("address")
            .value_name("n")
            .value_parser(parse_flash_u32)
            .help("starting address (default: 0)"),
        clap::Arg::new("length")
            .short('l')
            .long("length")
            .value_name("n")
            .value_parser(parse_flash_u32)
            .help("number of bytes to read (default: 0)"),
        clap::Arg::new("read")
            .short('r')
            .long("read")
            .value_name("filename")
            .help("Read data into file (SPIFI@0x80000000)"),
        clap::Arg::new("write")
            .short('w')
            .long("write")
            .value_name("filename")
            .help("Write data from file"),
    ]
}

/// `-s`, `--help`, and the `--force` confirmation deviation.
fn control_args() -> Vec<clap::Arg> {
    vec![
        crate::serial_arg(),
        clap::Arg::new("help")
            .long("help")
            .action(clap::ArgAction::Help)
            .help("Print help"),
        clap::Arg::new("force")
            .long("force")
            .action(clap::ArgAction::SetTrue)
            .help("confirm flash writes (-w) — they erase and rewrite the firmware"),
    ]
}
