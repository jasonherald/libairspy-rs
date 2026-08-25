//! Shared logic for the `airspy_spiflash` binary, ported from
//! `airspy-tools/src/airspy_spiflash.c`: the flash-size bound, the
//! 256-byte transfer chunking, and the range validation. The binary
//! holds the C print formats and the device/file wiring.

/// `MAX_LENGTH` in `airspy_spiflash.c` — "8 Mbit flash" (1 MiB).
pub const MAX_LENGTH: u32 = 0x0010_0000;

/// The 256-byte transfer size in the C read/write loops
/// (`xfer_len = (tmp_length > 256) ? 256 : tmp_length`).
const XFER_CHUNK: u32 = 256;

/// Why a requested range is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashRangeError {
    /// C's "Requested transfer of zero bytes." check.
    ZeroLength,
    /// C's "Request exceeds size of flash memory." check — with the
    /// address included (deviation: C tests only `length >
    /// MAX_LENGTH`, so an offset transfer could erase and then fail
    /// partway through the writes).
    ExceedsFlash,
}

/// The pre-transfer validation from C `main()`, address-inclusive.
pub fn validate_range(address: u32, length: u32) -> Result<(), FlashRangeError> {
    if length == 0 {
        return Err(FlashRangeError::ZeroLength);
    }
    if u64::from(address) + u64::from(length) > u64::from(MAX_LENGTH) {
        return Err(FlashRangeError::ExceedsFlash);
    }
    Ok(())
}

/// The C read/write loop shape: `(address, xfer_len)` pairs walking
/// the range in 256-byte (`XFER_CHUNK`) transfers.
pub fn transfer_chunks(address: u32, length: u32) -> impl Iterator<Item = (u32, u16)> {
    let mut address = address;
    let mut remaining = length;
    std::iter::from_fn(move || {
        if remaining == 0 {
            return None;
        }
        let xfer = remaining.min(XFER_CHUNK);
        let chunk = (address, u16::try_from(xfer).unwrap_or(u16::MAX));
        address += xfer;
        remaining -= xfer;
        Some(chunk)
    })
}

// The clap builders intentionally follow this module — see the
// Lizard note below.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_length_matches_c_define() {
        // "8 Mbit flash" — MAX_LENGTH 0x100000 in airspy_spiflash.c.
        assert_eq!(MAX_LENGTH, 0x0010_0000);
    }

    #[test]
    fn chunks_walk_the_range_in_256_byte_transfers() {
        // The C read/write loops: xfer_len = min(remaining, 256),
        // advancing address and remaining together.
        let chunks: Vec<(u32, u16)> = transfer_chunks(0x100, 600).collect();
        assert_eq!(chunks, [(0x100, 256), (0x200, 256), (0x300, 88)]);
        let chunks: Vec<(u32, u16)> = transfer_chunks(0, 256).collect();
        assert_eq!(chunks, [(0, 256)]);
        assert_eq!(transfer_chunks(5, 0).count(), 0);
    }

    #[test]
    fn range_validation_covers_c_checks_and_the_address_fix() {
        // C checks length == 0 and length > MAX_LENGTH; deviation:
        // the flash-size check includes the address, so a transfer
        // cannot run off the end of the flash mid-operation (C would
        // erase, then fail partway through the writes).
        assert_eq!(validate_range(0, 0), Err(FlashRangeError::ZeroLength));
        assert_eq!(validate_range(0, MAX_LENGTH), Ok(()));
        assert_eq!(
            validate_range(1, MAX_LENGTH),
            Err(FlashRangeError::ExceedsFlash)
        );
        assert_eq!(
            validate_range(0x000F_0000, 0x0002_0000),
            Err(FlashRangeError::ExceedsFlash)
        );
        assert_eq!(validate_range(0x000F_0000, 0x0001_0000), Ok(()));
        // u32 overflow cannot sneak past the check.
        assert_eq!(
            validate_range(u32::MAX, u32::MAX),
            Err(FlashRangeError::ExceedsFlash)
        );
    }

    #[test]
    fn command_mirrors_c_getopt_flags() {
        // getopt_long "a:l:r:w:s:" — the C long_options table also
        // lists { "reset", 't' }, but the option string and switch
        // have no 't', so --reset only ever hits C's error path;
        // deviation: it is omitted here.
        let matches = flash_command()
            .try_get_matches_from(["t", "-a", "0x1000", "-l", "256", "-r", "dump.bin"])
            .expect("parse");
        assert_eq!(matches.get_one::<u32>("address"), Some(&0x1000));
        assert_eq!(matches.get_one::<u32>("length"), Some(&256));
        assert_eq!(
            matches.get_one::<String>("read").map(String::as_str),
            Some("dump.bin")
        );
        assert!(
            flash_command()
                .try_get_matches_from(["t", "--reset"])
                .is_err()
        );
        // --force is the write-confirmation deviation.
        let matches = flash_command()
            .try_get_matches_from(["t", "-w", "fw.bin", "--force"])
            .expect("parse");
        assert!(matches.get_flag("force"));
    }
}

// NOTE: the clap builders sit below the test module deliberately.
// Codacy's Lizard mis-lexes Rust char literals (`.short('a')` reads
// as a lifetime plus a dangling quote), swallowing every following
// function into one giant measurement; keeping these last confines
// the confusion to this tail. Fix belongs upstream in Lizard.

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
            .value_parser(crate::rx::parse_u32)
            .help("starting address (default: 0)"),
        clap::Arg::new("length")
            .short('l')
            .long("length")
            .value_name("n")
            .value_parser(crate::rx::parse_u32)
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
