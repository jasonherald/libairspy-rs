//! Port of `airspy_gpiodir.c`: read and write GPIO pin directions
//! (0 = input, 1 = output). Output goes to stdout with the C tool's
//! exact formats so the two are diffable.

use airspy_tools::gpio_cli::{
    PIN_NUM_MAX, PIN_NUM_MIN, PORT_NUM_MAX, PORT_NUM_MIN, ReadScope, ReplayOutcome, dump_scope,
    gpio_command, ops_from_matches, replay_ops,
};
use libairspy_rs::commands::{GpioPin, GpioPort};
use libairspy_rs::{Device, Error};

/// The C tool's `usage()`, including its reconfiguration warning.
fn usage() {
    println!(
        "WARNING this tool reconfigure GPIO Direction IN/OUT and can destroy GPIO/MCU in case of mistake"
    );
    println!("Usage:");
    println!(
        "\t-p, --port_no <p>: set port number<p>[{PORT_NUM_MIN},{PORT_NUM_MAX}] for subsequent read/write operations"
    );
    println!(
        "\t-n, --pin_no <n>: set pin number<n>[{PIN_NUM_MIN},{PIN_NUM_MAX}] for subsequent read/write operations"
    );
    println!(
        "\t-r, --read: read port number/pin number direction specified by last -n argument, or all port/pin"
    );
    println!(
        "\t-w, --write <v>: write value port direction specified by last -n argument with value<v>[0,1](0=IN,1=OUT)"
    );
    println!("\t[-s serial_number_64bits]: Open board with specified 64bits serial number.");
    println!("\nExamples:");
    println!("\t<command> -p 0 -n 12 -r # reads gpio direction from port 0 pin number 12");
    println!("\t<command> -r          # reads gpio direction on all pins and all ports");
    println!(
        "\t<command> -p 0 -n 10 -w 1 # writes gpio direction port 0 pin number 10 with 1(output) decimal"
    );
}

/// `dump_port_pin` in `airspy_gpiodir.c`.
fn dump_port_pin(device: &Device, port: GpioPort, pin: GpioPin) -> Result<(), Error> {
    match device.gpiodir_read(port, pin) {
        Ok(1) => {
            println!("gpiodir[{}][{:2}] -> out(1)", port as u8, pin as u8);
            Ok(())
        }
        Ok(_) => {
            println!("gpiodir[{}][{:2}] -> in(0)", port as u8, pin as u8);
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_gpiodir_read() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

/// `write_port_pin` in `airspy_gpiodir.c`.
fn write_port_pin(device: &Device, port: GpioPort, pin: GpioPin, value: u8) -> Result<(), Error> {
    match device.gpiodir_write(port, pin, value) {
        Ok(()) => {
            println!(
                "0x{:02X} -> gpiodir[{}][{:2}]",
                value, port as u8, pin as u8
            );
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_gpiodir_write() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

fn main() {
    let matches = gpio_command(
        "airspy_gpiodir",
        "Read and write Airspy GPIO pin directions",
    )
    .get_matches();

    // C prints the serial line in its first getopt pass, before open.
    let serial = matches.get_one::<u64>("serial").copied();
    if let Some(serial) = serial {
        println!(
            "Board serial number to open: 0x{:08X}{:08X}",
            (serial >> 32) as u32,
            (serial & 0xFFFF_FFFF) as u32
        );
    }

    let open_result = match serial {
        Some(serial) => (Device::open_serial(serial), "airspy_open_sn()"),
        None => (Device::open(), "airspy_open()"),
    };
    let device = match open_result {
        (Ok(device), _) => device,
        (Err(err), context) => {
            println!("{context} failed: {} ({})", err.name(), err.code());
            usage();
            std::process::exit(1);
        }
    };

    let ops = ops_from_matches(&matches);
    let outcome = replay_ops(
        &ops,
        &mut |scope: ReadScope| {
            dump_scope(scope, &mut |port, pin| dump_port_pin(&device, port, pin))
        },
        &mut |port, pin, value| write_port_pin(&device, port, pin, value),
        &mut |line| println!("{line}"),
    );

    if outcome != ReplayOutcome::Completed {
        usage();
    }
    // Deviation: C exits 0 even when an operation failed; a failed
    // operation surfaces as status 1 here.
    std::process::exit(i32::from(outcome == ReplayOutcome::Failed));
}
