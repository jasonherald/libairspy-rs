//! Port of `airspy_si5351c.c`: read, write, and dump the si5351c
//! clock generator's registers, plus the `-c` multisynth
//! configuration decode. Output goes to stdout with the C tool's
//! exact formats so the two are diffable.

use airspy_tools::gpio_cli::{ReplayOutcome, open_from_matches};
use airspy_tools::reg_cli::{
    DIV_LUT, RegReadScope, SI5351C_SPEC, dump_all_registers, ms_int_output_mhz, ms_output_mhz,
    ms_params, reg_command, reg_ops_from_matches, replay_reg_ops,
};
use libairspy_rs::{Device, Error};

/// `reg_base = 42 + (ms_number * 8)` in `dump_multisynth_config`.
const MS_FRACTIONAL_REG_BASE: u8 = 42;
/// The MS6/MS7 register base (`reg_base = 90`).
const MS_INTEGER_REG_BASE: u8 = 90;

/// The C tool's `usage()` (note its leading blank line).
fn usage() {
    println!("\nUsage:");
    println!("\t-c, --config: print textual configuration information");
    println!("\t-n, --register <n>: set register number for subsequent read/write operations");
    println!("\t-r, --read: read register specified by last -n argument, or all registers");
    println!("\t-w, --write <v>: write register specified by last -n argument with value <v>");
    println!("\t[-s serial_number_64bits]: Open board with specified 64bits serial number.");
    println!("\nExamples:");
    println!("\t<command> -n 12 -r    # reads from register 12");
    println!("\t<command> -r          # reads all registers");
    println!("\t<command> -n 10 -w 22 # writes register 10 with 22 decimal");
}

/// `dump_register` in `airspy_si5351c.c` (lowercase hex).
fn dump_register(device: &Device, register: u8) -> Result<(), Error> {
    match device.si5351c_read(register) {
        Ok(value) => {
            println!("[{register:3}] -> 0x{value:02x}");
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_si5351c_read() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

/// `write_register` in `airspy_si5351c.c` — note C's `%2x`
/// (space-padded, lowercase, no `0` fill).
fn write_register(device: &Device, register: u8, value: u8) -> Result<(), Error> {
    match device.si5351c_write(register, value) {
        Ok(()) => {
            println!("0x{value:2x} -> [{register:3}]");
            Ok(())
        }
        Err(err) => {
            println!(
                "airspy_si5351c_write() failed: {} ({})",
                err.name(),
                err.code()
            );
            Err(err)
        }
    }
}

/// `dump_multisynth_config` in `airspy_si5351c.c`. A failed register
/// read returns silently mid-print exactly as C does (the pending
/// `MS%d:` stays unterminated before `usage()`).
fn dump_multisynth_config(device: &Device, ms_number: u8) -> Result<(), Error> {
    print!("MS{ms_number}:");
    let r_div;
    if ms_number < 6 {
        let reg_base = MS_FRACTIONAL_REG_BASE + ms_number * 8;
        let mut parameters = [0u8; 8];
        for (i, slot) in parameters.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let register = reg_base + i as u8;
            *slot = device.si5351c_read(register)?;
        }
        let params = ms_params(&parameters);
        r_div = params.r_div;
        println!("\tp1 = {}", params.p1);
        println!("\tp2 = {}", params.p2);
        println!("\tp3 = {}", params.p3);
        if params.p3 != 0 {
            println!("\tOutput (800Mhz PLL): {:.10} Mhz", ms_output_mhz(&params));
        }
    } else {
        // MS6 and 7 are integer only.
        let mut parameters = [0u8; 3];
        for (i, slot) in parameters.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let register = MS_INTEGER_REG_BASE + i as u8;
            *slot = device.si5351c_read(register)?;
        }
        r_div = if ms_number == 6 {
            u32::from(parameters[2] & 0x7)
        } else {
            u32::from((parameters[2] & 0x70) >> 4)
        };
        let parms = if ms_number == 6 {
            parameters[0]
        } else {
            parameters[1]
        };
        println!("\tp1_int = {parms}");
        if parms != 0 {
            println!(
                "\tOutput (800Mhz PLL): {:.10} Mhz",
                f64::from(ms_int_output_mhz(parms, r_div))
            );
        }
    }
    println!("\toutput divider = {}", DIV_LUT[r_div as usize]);
    Ok(())
}

/// `dump_configuration` in `airspy_si5351c.c`: all eight multisynth
/// blocks, stopping at the first failure.
fn dump_configuration(device: &Device) -> Result<(), Error> {
    for ms_number in 0..8 {
        dump_multisynth_config(device, ms_number)?;
    }
    Ok(())
}

fn main() {
    airspy_tools::reset_sigpipe();
    let matches = reg_command(
        "airspy_si5351c",
        "Read and write the Airspy si5351c clock generator registers",
        true,
    )
    .get_matches();

    let Some(device) = open_from_matches(&matches) else {
        usage();
        std::process::exit(1);
    };

    let ops = reg_ops_from_matches(&matches);
    let outcome = replay_reg_ops(
        SI5351C_SPEC,
        &ops,
        &mut |scope| match scope {
            RegReadScope::All => dump_all_registers(SI5351C_SPEC.register_max, &mut |register| {
                dump_register(&device, register)
            }),
            RegReadScope::Single(register) => dump_register(&device, register),
        },
        &mut |register, value| write_register(&device, register, value),
        &mut || dump_configuration(&device),
        &mut |line| println!("{line}"),
    );

    if outcome != ReplayOutcome::Completed {
        usage();
    }
    // Deviation: C exits 0 even when an operation failed; a failed
    // operation surfaces as status 1 here.
    std::process::exit(i32::from(outcome == ReplayOutcome::Failed));
}
