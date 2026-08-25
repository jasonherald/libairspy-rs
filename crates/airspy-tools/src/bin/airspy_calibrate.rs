//! Port of `airspy_calibrate.c`: read or write the calibration
//! record stored in the SPI flash (offset 0x20000, sector 2).
//! Output streams and formats track the C tool so the two are
//! diffable.

use airspy_tools::calib_args::calib_command;
use airspy_tools::calib_cli::{
    CALIB_HEADER, CALIB_LEN, CALIB_OFFSET, CALIB_SECTOR, Calib, format_calib_time,
};
use libairspy_rs::Device;

/// The C tool's `usage()` (stdout).
fn usage() {
    println!("Usage:");
    println!("\t-r: Read and display calibration data.");
    println!("\t-w <calibration in ppb>: Erase and Write calibration in ppb.");
}

/// The shared timestamp + ppb print of the C read and write paths.
fn print_calib(calib: &Calib) {
    let local = chrono::DateTime::from_timestamp(i64::from(calib.timestamp), 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local);
    println!(
        "Calibration timestamp: {}\nCalibration correction in ppb: {}",
        format_calib_time(&local),
        calib.correction_ppb
    );
}

/// The `-r` path: one 12-byte flash read, then the display.
fn read_calibration(device: &Device) -> Result<(), ()> {
    println!("Reading {CALIB_LEN} bytes from 0x{CALIB_OFFSET:06x}.");
    let mut raw = [0u8; CALIB_LEN];
    if let Err(err) = device.spiflash_read(CALIB_OFFSET, &mut raw) {
        eprintln!(
            "airspy_spiflash_read() failed: {} ({})",
            err.name(),
            err.code()
        );
        return Err(());
    }
    let calib = Calib::from_le_bytes(&raw);
    // Deviation: C prints whatever it read; warn when the magic is
    // missing (erased flash reads as all 0xFF) but display as C does.
    if !calib.header_valid() {
        eprintln!(
            "warning: calibration header 0x{:08X} does not match 0x{CALIB_HEADER:08X}; the record may be unprogrammed",
            calib.header
        );
    }
    print_calib(&calib);
    Ok(())
}

/// The `-w` path: erase sector 2, then write the fresh record —
/// printing what will be written first, as C does.
fn write_calibration(device: &Device, correction_ppb: i32) -> Result<(), ()> {
    println!("Erasing sector 2 (calibration) in SPI flash.");
    if let Err(_err) = device.spiflash_erase_sector(CALIB_SECTOR) {
        eprintln!("Failed to erase sector 2.");
        return Err(());
    }

    // C: calib.timestamp = (uint32_t)time(NULL).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let calib = Calib {
        header: CALIB_HEADER,
        timestamp: chrono::Utc::now().timestamp() as u32,
        correction_ppb,
    };
    println!("Writing calibration {CALIB_LEN} bytes at 0x{CALIB_OFFSET:06x}.");
    print_calib(&calib);
    if let Err(_err) = device.spiflash_write(CALIB_OFFSET, &calib.to_le_bytes()) {
        eprintln!("Failed to write calibration data.");
        return Err(());
    }
    Ok(())
}

fn main() {
    let matches = calib_command().get_matches();
    let read = matches.get_flag("read");
    let write = matches.get_one::<i32>("write").copied();

    // C: `if (write == read)` — both or neither.
    match (read, write) {
        (true, Some(_)) => {
            eprintln!("Read and write options are mutually exclusive.");
            usage();
            std::process::exit(1);
        }
        (false, None) => {
            eprintln!("Specify either read or write option.");
            usage();
            std::process::exit(1);
        }
        _ => {}
    }

    // Deviation: a calibration write erases sector 2 and rewrites
    // the record, so it requires explicit confirmation (checked
    // before the device opens; reads are untouched).
    if write.is_some() && !matches.get_flag("force") {
        eprintln!(
            "error: -w erases the calibration sector and rewrites it; re-run with --force to confirm"
        );
        usage();
        std::process::exit(1);
    }

    // C: this tool opens without a serial option and prints its own
    // failure message.
    let device = match Device::open() {
        Ok(device) => device,
        Err(_err) => {
            eprintln!("Failed to open airspy device.");
            std::process::exit(1);
        }
    };

    let result = match write {
        Some(correction_ppb) => write_calibration(&device, correction_ppb),
        None => read_calibration(&device),
    };
    if result.is_err() {
        std::process::exit(1);
    }
}
