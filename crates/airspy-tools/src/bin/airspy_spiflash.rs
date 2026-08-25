//! Port of `airspy_spiflash.c`: read the SPI flash into a file, or
//! erase and rewrite it from a file (the firmware-update path).
//! Output streams and formats track the C tool (status on stdout,
//! errors on stderr) so the two are diffable.

use std::io::{Read, Write};

use airspy_tools::flash_args::flash_command;
use airspy_tools::flash_cli::{FlashRangeError, MAX_LENGTH, transfer_chunks, validate_range};
use airspy_tools::gpio_cli::open_from_matches;
use libairspy_rs::Device;

/// The C tool's `usage()` (stdout).
fn usage() {
    println!("Usage:");
    println!("\t-a, --address <n>: starting address (default: 0)");
    println!("\t-l, --length <n>: number of bytes to read (default: 0)");
    println!("\t-r <filename>: Read data into file (SPIFI@0x80000000).");
    println!("\t-w <filename>: Write data from file.");
    println!("\t[-s serial_number_64bits]: Open board with specified 64bits serial number.");
}

/// Print a range error with C's stderr messages.
fn print_range_error(error: FlashRangeError, address: u32, length: u64) {
    match error {
        FlashRangeError::ZeroLength => eprintln!("Requested transfer of zero bytes."),
        FlashRangeError::ExceedsFlash => {
            eprintln!("Request exceeds size of flash memory.");
            eprintln!("address=0x{address:08X} size={length} Bytes.");
        }
    }
}

/// The read path: chunked `airspy_spiflash_read` into a buffer, then
/// one file write, exactly as C stages it.
fn read_flash(device: &Device, path: &str, address: u32, length: u32) -> Result<(), ()> {
    let Ok(mut file) = std::fs::File::create(path) else {
        // C: fopen("wb") failure prints to stdout.
        println!("Error to open file {path}");
        return Err(());
    };
    let mut data = vec![0u8; length as usize];
    let mut offset = 0usize;
    for (chunk_address, xfer_len) in transfer_chunks(address, length) {
        println!("Reading {xfer_len} bytes from 0x{chunk_address:06x}.");
        let end = offset + usize::from(xfer_len);
        if let Err(err) = device.spiflash_read(chunk_address, &mut data[offset..end]) {
            eprintln!(
                "airspy_spiflash_read() failed: {} ({})",
                err.name(),
                err.code()
            );
            return Err(());
        }
        offset = end;
    }
    if file.write_all(&data).is_err() {
        eprintln!("Failed write to file (wrote 0 bytes).");
        return Err(());
    }
    Ok(())
}

/// The write path: erase, then chunked `airspy_spiflash_write` from
/// the already-loaded file contents.
fn write_flash(device: &Device, address: u32, data: &[u8]) -> Result<(), ()> {
    // C's message verbatim (the erase clears the whole flash; the
    // "1st 64KB" wording is the C tool's).
    println!("Erasing 1st 64KB in SPI flash.");
    if let Err(err) = device.spiflash_erase() {
        eprintln!(
            "airspy_spiflash_erase() failed: {} ({})",
            err.name(),
            err.code()
        );
        return Err(());
    }
    let mut offset = 0usize;
    #[allow(clippy::cast_possible_truncation)]
    for (chunk_address, xfer_len) in transfer_chunks(address, data.len() as u32) {
        println!("Writing {xfer_len} bytes at 0x{chunk_address:06x}.");
        let end = offset + usize::from(xfer_len);
        if let Err(err) = device.spiflash_write(chunk_address, &data[offset..end]) {
            eprintln!(
                "airspy_spiflash_write() failed: {} ({})",
                err.name(),
                err.code()
            );
            return Err(());
        }
        offset = end;
    }
    Ok(())
}

/// The `if (write == read)` exclusivity check and `--force` gate:
/// returns the path and whether this is a write.
fn resolve_operation(matches: &clap::ArgMatches) -> (String, bool) {
    let read_path = matches.get_one::<String>("read");
    let write_path = matches.get_one::<String>("write");
    // C: `if (write == read)` — both or neither.
    let path = match (read_path, write_path) {
        (Some(_), Some(_)) => {
            eprintln!("Read and write options are mutually exclusive.");
            usage();
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!("Specify either read or write option.");
            usage();
            std::process::exit(1);
        }
        (Some(path), None) | (None, Some(path)) => path.clone(),
    };
    let writing = write_path.is_some();
    // Deviation: a flash write erases and rewrites the firmware, so
    // it requires explicit confirmation (checked before any device
    // or file access; reads are untouched).
    if writing && !matches.get_flag("force") {
        eprintln!(
            "error: -w erases and rewrites the SPI flash firmware; re-run with --force to confirm"
        );
        usage();
        std::process::exit(1);
    }
    (path, writing)
}

/// The write-path file staging, in C's exact order: size first (C's
/// fseek/ftell — metadata here), the "File size" print, the range
/// checks, and only then a bounded read of exactly `length` bytes.
/// Validating before reading also bounds the allocation, so an
/// unbounded input like /dev/zero is rejected at size 0 as in C.
fn load_write_file(path: &str, address: u32) -> (Vec<u8>, u32) {
    let Ok(file) = std::fs::File::open(path) else {
        // C: fopen("rb") failure prints to stdout.
        println!("Error to open file {path}");
        std::process::exit(1);
    };
    let file_len = file.metadata().map_or(0, |m| m.len());
    println!("File size {file_len} bytes.");
    // Deviation: C stores ftell into a uint32_t, so a >4 GiB file
    // wraps past the size check; the real length is checked before
    // it ever narrows.
    let length = match u32::try_from(file_len) {
        Ok(length) if length <= MAX_LENGTH => length,
        _ => {
            print_range_error(FlashRangeError::ExceedsFlash, address, file_len);
            usage();
            std::process::exit(1);
        }
    };
    if let Err(error) = validate_range(address, length) {
        print_range_error(error, address, u64::from(length));
        usage();
        std::process::exit(1);
    }
    let mut file_data = Vec::with_capacity(length as usize);
    let read = file
        .take(u64::from(length))
        .read_to_end(&mut file_data)
        .unwrap_or(0);
    if read != length as usize {
        // C: fread short count → "Failed read file (read %d bytes)."
        eprintln!("Failed read file (read {read} bytes).");
        std::process::exit(1);
    }
    (file_data, length)
}

fn main() {
    let matches = flash_command().get_matches();
    let address = matches.get_one::<u32>("address").copied().unwrap_or(0);
    let mut length = matches.get_one::<u32>("length").copied().unwrap_or(0);
    let (path, writing) = resolve_operation(&matches);

    // For writes C takes the length from the file size (validated
    // inside load_write_file, in C's order).
    let mut file_data = Vec::new();
    if writing {
        (file_data, length) = load_write_file(&path, address);
    } else if let Err(error) = validate_range(address, length) {
        print_range_error(error, address, u64::from(length));
        usage();
        std::process::exit(1);
    }

    let Some(device) = open_from_matches(&matches) else {
        usage();
        std::process::exit(1);
    };

    let result = if writing {
        write_flash(&device, address, &file_data)
    } else {
        read_flash(&device, &path, address, length)
    };
    if result.is_err() {
        std::process::exit(1);
    }
}
