//! Port of `airspy_gpio.c`: read and write GPIO pin levels (reads
//! also show the pin direction). Output goes to stdout with the C
//! tool's exact formats so the two are diffable.

use airspy_tools::gpio_cli::{
    PIN_NUM_MAX, PIN_NUM_MIN, PORT_NUM_MAX, PORT_NUM_MIN, ReadScope, ReplayOutcome, dump_scope,
    gpio_command, ops_from_matches, replay_ops,
};
use libairspy_rs::commands::{GpioPin, GpioPort};
use libairspy_rs::{Device, Error};

/// The C tool's `usage()` (stdout, like every print in this tool).
fn usage() {
    println!("Usage:");
    println!(
        "\t-p, --port_no <p>: set port number<p>[{PORT_NUM_MIN},{PORT_NUM_MAX}] for subsequent read/write operations"
    );
    println!(
        "\t-n, --pin_no <n>: set pin number<n>[{PIN_NUM_MIN},{PIN_NUM_MAX}] for subsequent read/write operations"
    );
    println!(
        "\t-r, --read: read port number/pin number value and direction specified by last -n argument, or all port/pin"
    );
    println!("\t-w, --write <v>: write value specified by last -n argument with value<v>[0,1]");
    println!("\t[-s serial_number_64bits]: Open board with specified 64bits serial number.");
    println!("\nExamples:");
    println!("\t<command> -p 0 -n 12 -r # reads from port 0 pin number 12");
    println!("\t<command> -r          # reads all pins on all ports");
    println!("\t<command> -p 0 -n 10 -w 1 # writes port 0 pin number 10 with 1 decimal");
    println!("\nHardware Info AirSpy:");
    println!("LED1(out): -p 0 -n 12 (0=OFF, 1=ON)");
    println!("Enable R820T(out): -p 1 -n 7 (0=OFF, 1=ON)");
    println!("Enable BiasT(out): -p 1 -n 13 (0=OFF, 1=ON)");
}

/// `dump_port_pin` in `airspy_gpio.c`: the level, then the direction
/// appended on the same line (`printf` without newline before the
/// direction read).
fn dump_port_pin(device: &Device, port: GpioPort, pin: GpioPin) -> Result<(), Error> {
    match device.gpio_read(port, pin) {
        Ok(value) => {
            print!("gpio[{}][{:2}] -> 0x{:02X}", port as u8, pin as u8, value);
            match device.gpiodir_read(port, pin) {
                Ok(1) => {
                    println!(" out(1)");
                    Ok(())
                }
                Ok(_) => {
                    println!(" in(0)");
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
        Err(err) => {
            println!("airspy_gpio_read() failed: {} ({})", err.name(), err.code());
            Err(err)
        }
    }
}

/// `write_port_pin` in `airspy_gpio.c`.
fn write_port_pin(device: &Device, port: GpioPort, pin: GpioPin, value: u8) -> Result<(), Error> {
    match device.gpio_write(port, pin, value) {
        Ok(()) => {
            println!("0x{:02X} -> gpio[{}][{:2}]", value, port as u8, pin as u8);
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_gpio_write() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

fn main() {
    let matches = gpio_command("airspy_gpio", "Read and write Airspy GPIO pins").get_matches();

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
