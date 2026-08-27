//! Port of `airspy_info.c`: enumerate Airspy boards and print their
//! identity — board id, firmware version, part id, serial number, and
//! supported sample rates. Output format tracks the C tool so the two
//! can be diffed against the same hardware.

use airspy_tools::parse_u64;
use clap::Parser;
use libairspy_rs::commands::BoardId;
use libairspy_rs::{Device, Error, lib_version};

/// `AIRSPY_MAX_DEVICE` in `airspy_info.c`.
const MAX_DEVICES: usize = 32;

/// The `0.000001f` Hz→MSPS factor in `airspy_info.c`'s sample-rate
/// printout, kept in f32 to match the C tool's float arithmetic.
const HZ_TO_MSPS: f32 = 0.000_001;

#[derive(Parser)]
#[command(
    name = "airspy_info",
    about = "Print information about attached Airspy boards"
)]
struct Args {
    /// Open board with specified 64bits serial number
    #[arg(short = 's', value_name = "serial_number_64bits", value_parser = parse_u64)]
    serial_number: Option<u64>,
}

fn print_error(context: &str, err: &Error) {
    eprintln!("{context} failed: {} ({})", err.name(), err.code());
}

fn main() {
    airspy_tools::reset_sigpipe();
    let args = Args::parse();

    if let Some(serial) = args.serial_number {
        // C: "Board serial number to open: 0x%08X%08X"
        println!(
            "Board serial number to open: 0x{:08X}{:08X}",
            (serial >> 32) as u32,
            (serial & 0xFFFF_FFFF) as u32
        );
    }

    let v = lib_version();
    println!("airspy_lib_version: {}.{}.{}", v.major, v.minor, v.revision);

    // Open every reachable board (each successful open claims its
    // interface, so repeated opens walk the device list like C's loop).
    let mut devices = Vec::new();
    while devices.len() < MAX_DEVICES {
        let result = match args.serial_number {
            Some(serial) => Device::open_serial(serial),
            None => Device::open(),
        };
        match result {
            Ok(device) => devices.push(device),
            Err(err) => {
                if devices.is_empty() {
                    print_error(&format!("airspy_open() board {}", 1), &err);
                }
                break;
            }
        }
    }

    // Consume the vec so each Device drops (closing its handle) at the
    // end of its iteration — C calls airspy_close inside this loop.
    for (i, device) in devices.into_iter().enumerate() {
        let board_num = i + 1;
        println!("\nFound AirSpy board {board_num}");

        match device.board_id() {
            Ok(board_id) => {
                let name = BoardId::from_u8(board_id).map_or("Unknown Board ID", BoardId::name);
                println!("Board ID Number: {board_id} ({name})");
            }
            Err(err) => {
                print_error("airspy_board_id_read()", &err);
                continue;
            }
        }

        match device.version_string() {
            Ok(version) => println!("Firmware Version: {version}"),
            Err(err) => {
                print_error("airspy_version_string_read()", &err);
                continue;
            }
        }

        match device.partid_serialno() {
            Ok(ids) => {
                println!(
                    "Part ID Number: 0x{:08X} 0x{:08X}",
                    ids.part_id[0], ids.part_id[1]
                );
                println!(
                    "Serial Number: 0x{:08X}{:08X}",
                    ids.serial_no[2], ids.serial_no[3]
                );
            }
            Err(err) => {
                print_error("airspy_board_partid_serialno_read()", &err);
                continue;
            }
        }

        println!("Supported sample rates:");
        for rate in device.samplerates() {
            // C: "\t%f MSPS".
            #[allow(clippy::cast_precision_loss)]
            let msps = rate as f32 * HZ_TO_MSPS;
            println!("\t{msps:.6} MSPS");
        }

        println!("Close board {board_num}");
    }
    // Devices close on drop (airspy_close semantics); like the C tool,
    // reaching this point is EXIT_SUCCESS even when no board was found.
}
