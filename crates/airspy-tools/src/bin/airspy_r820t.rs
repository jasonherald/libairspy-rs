//! Port of `airspy_r820t.c`: read, write, and dump the R820T tuner
//! registers, plus the `-c` default-mode register configuration.
//! Output goes to stdout with the C tool's exact formats so the two
//! are diffable.

use airspy_tools::gpio_cli::{ReplayOutcome, open_from_matches};
use airspy_tools::reg_cli::{
    R820T_SPEC, RegReadScope, dump_all_registers, reg_command, reg_ops_from_matches, replay_reg_ops,
};
use libairspy_rs::{Device, Error};

/// `CONF_R820T_START_REG` in `airspy_r820t.c`.
const CONF_R820T_START_REG: u8 = 5;

/// `conf_r820t` in `airspy_r820t.c` — the "default mode for test"
/// register values for registers 5 through 31.
const CONF_R820T: [u8; 27] = [
    0x12, 0x32, 0x75, // 05 to 07
    0xc0, 0x40, 0xd6, 0x6c, // 08 to 11
    0x40, 0x63, 0x75, 0x68, // 12 to 15
    0x6c, 0x83, 0x80, 0x00, // 16 to 19
    0x0f, 0x00, 0xc0, 0x30, // 20 to 23
    0x48, 0xcc, 0x60, 0x00, // 24 to 27
    0x54, 0xae, 0x4a, 0xc0, // 28 to 31
];

/// The C tool's `usage()`.
fn usage() {
    println!("Usage:");
    println!(
        "\t-n, --register <n>: set register <n>[0,{}] for subsequent read/write operations",
        R820T_SPEC.register_max
    );
    println!("\t-r, --read: read register specified by last -n argument, or all registers");
    println!(
        "\t-w, --write <v>: write register specified by last -n argument with value <v>[0,255]"
    );
    println!("\t-c, --config: configure registers to r820t default mode for test");
    println!("\t[-s serial_number_64bits]: Open board with specified 64bits serial number.");
    println!("\nExamples:");
    println!("\t<command> -n 12 -r    # reads from register 12");
    println!("\t<command> -r          # reads all registers");
    println!("\t<command> -n 10 -w 22 # writes register 10 with 22 decimal");
}

/// `dump_register` in `airspy_r820t.c` (uppercase hex).
fn dump_register(device: &Device, register: u8) -> Result<(), Error> {
    match device.r820t_read(register) {
        Ok(value) => {
            println!("[{register:3}] -> 0x{value:02X}");
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_r820t_read() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

/// `write_register` in `airspy_r820t.c` (zero-padded uppercase hex).
fn write_register(device: &Device, register: u8, value: u8) -> Result<(), Error> {
    match device.r820t_write(register, value) {
        Ok(()) => {
            println!("0x{value:02X} -> [{register:3}]");
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_r820t_write() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

/// `configure_registers` in `airspy_r820t.c`: write the default
/// table to registers 5..=31, printing each, stopping on failure.
fn configure_registers(device: &Device) -> Result<(), Error> {
    for (i, value) in CONF_R820T.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let register = CONF_R820T_START_REG + i as u8;
        write_register(device, register, *value)?;
    }
    Ok(())
}

fn main() {
    let matches = reg_command(
        "airspy_r820t",
        "Read and write the Airspy R820T tuner registers",
        false,
    )
    .get_matches();

    let Some(device) = open_from_matches(&matches) else {
        usage();
        std::process::exit(1);
    };

    let ops = reg_ops_from_matches(&matches);
    let outcome = replay_reg_ops(
        R820T_SPEC,
        &ops,
        &mut |scope| match scope {
            RegReadScope::All => dump_all_registers(R820T_SPEC.register_max, &mut |register| {
                dump_register(&device, register)
            }),
            RegReadScope::Single(register) => dump_register(&device, register),
        },
        &mut |register, value| write_register(&device, register, value),
        &mut || configure_registers(&device),
        &mut |line| println!("{line}"),
    );

    if outcome != ReplayOutcome::Completed {
        usage();
    }
    // Deviation: C exits 0 even when an operation failed; a failed
    // operation surfaces as status 1 here.
    std::process::exit(i32::from(outcome == ReplayOutcome::Failed));
}
