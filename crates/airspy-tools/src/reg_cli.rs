//! Shared engine for the `airspy_si5351c` and `airspy_r820t`
//! register tools, ported from `airspy-tools/src/airspy_si5351c.c`
//! and `airspy_r820t.c`: the order-sensitive `-n`/`-r`/`-w`/`-c`
//! replay of the C getopt loops, the break-on-error register dump,
//! and the si5351c multisynth parameter math. Device-facing print
//! formats stay in the binaries; everything here is hardware-free
//! and unit-tested.

use libairspy_rs::Error;

use crate::gpio_cli::{ReplayOutcome, parse_u8};

/// Per-tool differences between the two C register tools.
#[derive(Debug, Clone, Copy)]
pub struct RegToolSpec {
    /// `REGISTER_NUM_MAX` (31) in `airspy_r820t.c`; the si5351c tool
    /// accepts every 8-bit register (`dump_registers` walks 0..256).
    pub register_max: u8,
    /// `airspy_r820t.c` prints range messages for `-n` and `-w`;
    /// `airspy_si5351c.c`'s `parse_int` failures are silent (the
    /// loop just breaks into `usage()`).
    pub print_messages: bool,
}

/// `airspy_r820t.c`'s behavior.
pub const R820T_SPEC: RegToolSpec = RegToolSpec {
    register_max: 31,
    print_messages: true,
};

/// `airspy_si5351c.c`'s behavior.
pub const SI5351C_SPEC: RegToolSpec = RegToolSpec {
    register_max: 255,
    print_messages: false,
};

/// One replayed occurrence of the C getopt loop's options, in
/// command-line order. Values stay raw strings — C parses each at
/// its own loop iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegOp {
    /// `-n <register>` — latch the register for later operations.
    Register(String),
    /// `-r` — dump the latched register, or all registers.
    Read,
    /// `-w <value>` — write the latched register.
    Write(String),
    /// `-c` — the tool-specific configuration operation.
    Config,
}

/// What a `-r` acts on: no register latched yet → the full dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegReadScope {
    /// `dump_registers` — every register up to the tool's maximum.
    All,
    /// `dump_register` — one register.
    Single(u8),
}

/// `dump_registers` in both C tools: walk `0..=register_max` and
/// stop at the first failed read (unlike the GPIO dump, which
/// continues through failures).
pub fn dump_all_registers(
    register_max: u8,
    per_register: &mut impl FnMut(u8) -> Result<(), Error>,
) -> Result<(), Error> {
    for register in 0..=register_max {
        per_register(register)?;
    }
    Ok(())
}

/// Replay the C getopt loop: `-n` latches the register, `-r`/`-w`/
/// `-c` act immediately, and the first failure stops the loop (no
/// "argument error" line — the register tools' helpers print their
/// own failures, then `usage()` follows). `out` receives the range
/// messages when the spec prints them.
///
/// Deviations: a `-w` without `-n` errors instead of writing C's
/// default/sentinel register (0 for si5351c, 255 for r820t), and
/// out-of-range si5351c values reject instead of C's silent
/// `(uint8_t)` truncation in `parse_int`.
pub fn replay_reg_ops(
    spec: RegToolSpec,
    ops: &[RegOp],
    read: &mut impl FnMut(RegReadScope) -> Result<(), Error>,
    write: &mut impl FnMut(u8, u8) -> Result<(), Error>,
    config: &mut impl FnMut() -> Result<(), Error>,
    out: &mut impl FnMut(String),
) -> ReplayOutcome {
    let mut register: Option<u8> = None;
    let mut outcome = ReplayOutcome::NoOps;
    for op in ops {
        let ok = match op {
            RegOp::Register(raw) => {
                if let Some(value) = parse_u8(raw).filter(|v| *v <= spec.register_max) {
                    // C's case 'n' stores the parse result, so a
                    // bare -n suppresses usage().
                    register = Some(value);
                    outcome = ReplayOutcome::Completed;
                    true
                } else {
                    if spec.print_messages {
                        out(format!(
                            "Error parameter -n shall be between 0 and {}",
                            spec.register_max
                        ));
                    }
                    false
                }
            }
            RegOp::Read => {
                let scope = register.map_or(RegReadScope::All, RegReadScope::Single);
                run_operation(&read(scope), &mut outcome)
            }
            RegOp::Write(raw) => match (parse_u8(raw), register) {
                (Some(value), Some(register)) => {
                    run_operation(&write(register, value), &mut outcome)
                }
                (None, _) => {
                    if spec.print_messages {
                        out("Error parameter -w shall be between 0 and 255".into());
                    }
                    false
                }
                _ => {
                    out("error: -w requires -n".into());
                    false
                }
            },
            RegOp::Config => run_operation(&config(), &mut outcome),
        };
        if !ok {
            return ReplayOutcome::Failed;
        }
    }
    outcome
}

/// The success/failure tail shared by the operation arms: mark
/// progress or stop (the helpers have already printed any failure).
fn run_operation(result: &Result<(), Error>, outcome: &mut ReplayOutcome) -> bool {
    if result.is_ok() {
        *outcome = ReplayOutcome::Completed;
        true
    } else {
        false
    }
}

/// `div_lut` in `dump_multisynth_config` (`airspy_si5351c.c`) — the
/// output-divider power-of-two table.
pub const DIV_LUT: [u32; 8] = [1, 2, 4, 8, 16, 32, 64, 128];

/// The decoded multisynth parameters from `dump_multisynth_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsParams {
    /// The 18-bit `p1` field.
    pub p1: u32,
    /// The 20-bit `p2` field.
    pub p2: u32,
    /// The 20-bit `p3` field.
    pub p3: u32,
    /// The 3-bit output-divider index into [`DIV_LUT`].
    pub r_div: u32,
}

/// The MS0–MS5 bit unpacking in `dump_multisynth_config`
/// (`airspy_si5351c.c`): `p1` spans registers 2..5, `p2` the low
/// nibble of 5 plus 6..8, `p3` the high nibble of 5 plus 0..2, and
/// `r_div` bits 4..6 of register 2.
pub fn ms_params(parameters: &[u8; 8]) -> MsParams {
    let p = parameters.map(u32::from);
    MsParams {
        p1: ((p[2] & 0x03) << 16) | (p[3] << 8) | p[4],
        p2: ((p[5] & 0x0F) << 16) | (p[6] << 8) | p[7],
        p3: ((p[5] & 0xF0) << 12) | (p[0] << 8) | p[1],
        r_div: (p[2] >> 4) & 0x7,
    }
}

/// The fractional output frequency print in `dump_multisynth_config`:
/// `((double)800 / (((double)p1*p3 + p2 + 512*p3) / (double)(128*p3)))
/// / div_lut[r_div]`, with `512*p3` computed in integer arithmetic
/// first exactly as C does.
pub fn ms_output_mhz(params: &MsParams) -> f64 {
    let divider = (f64::from(params.p1) * f64::from(params.p3)
        + f64::from(params.p2)
        + f64::from(512 * params.p3))
        / f64::from(128 * params.p3);
    (800.0 / divider) / f64::from(DIV_LUT[params.r_div as usize])
}

/// The MS6/MS7 integer output print: `(800.0f / parms) /
/// div_lut[r_div]` — float math in C, kept in f32.
#[allow(clippy::cast_precision_loss)]
pub fn ms_int_output_mhz(parms: u8, r_div: u32) -> f32 {
    (800.0f32 / f32::from(parms)) / DIV_LUT[r_div as usize] as f32
}

/// The shared clap command for both register tools — the C
/// `getopt_long(argc, argv, "cn:rw:s:", long_options, ...)` table.
/// `serial_long` mirrors the C difference: `airspy_si5351c.c` lists
/// a `--serial` long option, `airspy_r820t.c` has only `-s`.
pub fn reg_command(name: &'static str, about: &'static str, serial_long: bool) -> clap::Command {
    let mut serial = clap::Arg::new("serial")
        .short('s')
        .value_name("serial_number_64bits")
        .value_parser(crate::parse_u64)
        .help("Open board with specified 64bits serial number");
    if serial_long {
        serial = serial.long("serial");
    }
    clap::Command::new(name)
        .about(about)
        .disable_help_flag(true)
        .arg(
            clap::Arg::new("help")
                .long("help")
                .action(clap::ArgAction::Help)
                .help("Print help"),
        )
        .arg(
            // Zero-value append so every occurrence keeps its index.
            clap::Arg::new("config")
                .short('c')
                .long("config")
                .action(clap::ArgAction::Append)
                .num_args(0)
                .default_missing_value("c")
                .help("the tool-specific configuration operation"),
        )
        .arg(
            clap::Arg::new("register")
                .short('n')
                .long("register")
                .value_name("n")
                .action(clap::ArgAction::Append)
                .help("set register number for subsequent read/write operations"),
        )
        .arg(
            clap::Arg::new("read")
                .short('r')
                .long("read")
                .action(clap::ArgAction::Append)
                .num_args(0)
                .default_missing_value("r")
                .help("read register specified by last -n argument, or all registers"),
        )
        .arg(
            clap::Arg::new("write")
                .short('w')
                .long("write")
                .value_name("v")
                .action(clap::ArgAction::Append)
                .help("write register specified by last -n argument with value <v>"),
        )
        .arg(serial)
}

/// Recover the C getopt loop's op sequence in command-line order
/// (clap argument indices), like the GPIO tools do.
pub fn reg_ops_from_matches(matches: &clap::ArgMatches) -> Vec<RegOp> {
    let mut ops: Vec<(usize, RegOp)> = Vec::new();
    for (id, make) in [
        ("register", RegOp::Register as fn(String) -> RegOp),
        ("write", RegOp::Write as fn(String) -> RegOp),
    ] {
        if let (Some(values), Some(indices)) =
            (matches.get_many::<String>(id), matches.indices_of(id))
        {
            ops.extend(indices.zip(values).map(|(i, v)| (i, make(v.clone()))));
        }
    }
    for (id, op) in [("read", RegOp::Read), ("config", RegOp::Config)] {
        if let Some(indices) = matches.indices_of(id) {
            ops.extend(indices.map(|i| (i, op.clone())));
        }
    }
    ops.sort_by_key(|(i, _)| *i);
    ops.into_iter().map(|(_, op)| op).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libairspy_rs::Error;

    /// Recording harness for one tool spec.
    fn run(spec: RegToolSpec, ops: &[RegOp]) -> (ReplayOutcome, Vec<String>, Vec<String>) {
        let log = std::cell::RefCell::new(Vec::new());
        let mut out = Vec::new();
        let outcome = replay_reg_ops(
            spec,
            ops,
            &mut |scope| {
                log.borrow_mut().push(format!("read {scope:?}"));
                Ok(())
            },
            &mut |register, value| {
                log.borrow_mut().push(format!("write {register} {value}"));
                Ok(())
            },
            &mut || {
                log.borrow_mut().push("config".into());
                Ok(())
            },
            &mut |line| out.push(line),
        );
        (outcome, log.into_inner(), out)
    }

    #[test]
    fn reg_ops_recover_command_line_order() {
        let matches = reg_command("t", "t", true)
            .try_get_matches_from(["t", "-n", "10", "-w", "22", "-c", "-n", "12", "-r"])
            .expect("parse");
        assert_eq!(
            reg_ops_from_matches(&matches),
            [
                RegOp::Register("10".into()),
                RegOp::Write("22".into()),
                RegOp::Config,
                RegOp::Register("12".into()),
                RegOp::Read,
            ]
        );
        // r820t has no --serial long option; si5351c does.
        assert!(
            reg_command("t", "t", false)
                .try_get_matches_from(["t", "--serial", "0x1"])
                .is_err()
        );
        assert!(
            reg_command("t", "t", true)
                .try_get_matches_from(["t", "--serial", "0x1"])
                .is_ok()
        );
    }

    #[test]
    fn specs_match_c_tools() {
        // airspy_r820t.c: REGISTER_NUM_MAX 31, range and -w messages;
        // airspy_si5351c.c: all 256 registers, silent parse failures.
        assert_eq!(
            (R820T_SPEC.register_max, R820T_SPEC.print_messages),
            (31, true)
        );
        assert_eq!(
            (SI5351C_SPEC.register_max, SI5351C_SPEC.print_messages),
            (255, false)
        );
    }

    #[test]
    fn read_uses_latched_register_or_dumps_all() {
        let (o, log, _) = run(R820T_SPEC, &[RegOp::Read]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert_eq!(log, ["read All"]);

        let (_, log, _) = run(
            R820T_SPEC,
            &[RegOp::Register("12".into()), RegOp::Read, RegOp::Read],
        );
        assert_eq!(log, ["read Single(12)", "read Single(12)"]);
    }

    #[test]
    fn ops_replay_in_command_line_order() {
        let (o, log, _) = run(
            SI5351C_SPEC,
            &[
                RegOp::Register("10".into()),
                RegOp::Write("22".into()),
                RegOp::Config,
                RegOp::Register("12".into()),
                RegOp::Read,
            ],
        );
        assert_eq!(o, ReplayOutcome::Completed);
        assert_eq!(log, ["write 10 22", "config", "read Single(12)"]);
    }

    #[test]
    fn r820t_register_range_error_prints_c_message() {
        // airspy_r820t.c case 'n': out-of-range prints the message,
        // sets result = OTHER, and the loop breaks.
        let (o, log, out) = run(R820T_SPEC, &[RegOp::Register("32".into()), RegOp::Read]);
        assert_eq!(o, ReplayOutcome::Failed);
        assert!(log.is_empty());
        assert_eq!(out, ["Error parameter -n shall be between 0 and 31"]);

        let (_, _, out) = run(R820T_SPEC, &[RegOp::Register("abc".into())]);
        assert_eq!(out, ["Error parameter -n shall be between 0 and 31"]);
    }

    #[test]
    fn r820t_write_parse_error_prints_c_message() {
        // airspy_r820t.c case 'w': parse failure prints the 0..255
        // message.
        let (o, _, out) = run(
            R820T_SPEC,
            &[RegOp::Register("5".into()), RegOp::Write("256".into())],
        );
        assert_eq!(o, ReplayOutcome::Failed);
        assert_eq!(out, ["Error parameter -w shall be between 0 and 255"]);
    }

    #[test]
    fn si5351c_parse_failures_are_silent_and_reject_truncation() {
        // airspy_si5351c.c's parse_int has no range check and casts
        // long to uint8 (-n 300 would poke register 44); deviation:
        // out-of-range rejects, silently like C's parse failures.
        let (o, log, out) = run(SI5351C_SPEC, &[RegOp::Register("300".into()), RegOp::Read]);
        assert_eq!(o, ReplayOutcome::Failed);
        assert!(log.is_empty());
        assert!(out.is_empty());

        let (o, _, out) = run(
            SI5351C_SPEC,
            &[RegOp::Register("10".into()), RegOp::Write("abc".into())],
        );
        assert_eq!(o, ReplayOutcome::Failed);
        assert!(out.is_empty());

        // Register 255 is a valid si5351c register (no C range check).
        let (o, log, _) = run(SI5351C_SPEC, &[RegOp::Register("255".into()), RegOp::Read]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert_eq!(log, ["read Single(255)"]);
    }

    #[test]
    fn write_requires_register_selection() {
        // Deviation: without -n, C writes register 255 (r820t's
        // REGISTER_INVALID sentinel) or register 0 (si5351c's
        // default) on the wire.
        for spec in [R820T_SPEC, SI5351C_SPEC] {
            let (o, log, out) = run(spec, &[RegOp::Write("1".into())]);
            assert_eq!(o, ReplayOutcome::Failed);
            assert!(log.is_empty());
            assert_eq!(out, ["error: -w requires -n"]);
        }
    }

    #[test]
    fn bare_register_suppresses_usage_and_no_ops_reports_noops() {
        // C's case 'n' stores the parse result, so a bare -n ends
        // with SUCCESS; no options at all leaves AIRSPY_ERROR_OTHER.
        let (o, log, _) = run(R820T_SPEC, &[RegOp::Register("3".into())]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert!(log.is_empty());

        let (o, _, _) = run(R820T_SPEC, &[]);
        assert_eq!(o, ReplayOutcome::NoOps);
    }

    #[test]
    fn failed_operations_stop_without_argument_error_line() {
        // Unlike the GPIO tools, the register tools print no
        // "argument error" line — the dump/write helpers already
        // printed, and the loop just breaks into usage().
        let mut out = Vec::new();
        let outcome = replay_reg_ops(
            R820T_SPEC,
            &[RegOp::Read, RegOp::Read],
            &mut |_| Err(Error::NotFound),
            &mut |_, _| Ok(()),
            &mut || Ok(()),
            &mut |line| out.push(line),
        );
        assert_eq!(outcome, ReplayOutcome::Failed);
        assert!(out.is_empty());
    }

    #[test]
    fn dump_all_breaks_on_first_error() {
        // dump_registers in both C tools stops at the first failed
        // read (unlike the GPIO dump, which continues).
        let mut calls = Vec::new();
        let result = dump_all_registers(31, &mut |register| {
            calls.push(register);
            if register == 2 {
                Err(Error::NotFound)
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(calls, [0, 1, 2]);

        let mut count = 0u32;
        let result = dump_all_registers(255, &mut |_| {
            count += 1;
            Ok(())
        });
        assert!(result.is_ok());
        assert_eq!(count, 256);
    }

    #[test]
    fn multisynth_params_decode_c_bit_packing() {
        // dump_multisynth_config in airspy_si5351c.c: an 800/20 MHz
        // divider (40) encodes as p1 = 128*40 - 512 = 4608, p2 = 0,
        // p3 = 1, r_div = 0.
        let params = ms_params(&[0x00, 0x01, 0x00, 0x12, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(
            (params.p1, params.p2, params.p3, params.r_div),
            (4608, 0, 1, 0)
        );
        let mhz = ms_output_mhz(&params);
        assert!((mhz - 20.0).abs() < 1e-9);

        // Bit packing across fields: p1 spans params[2..5], p2 low
        // nibble of [5] plus [6..8], p3 high nibble of [5] plus
        // [0..2], r_div bits 4..7 of [2].
        let params = ms_params(&[0x00, 0x01, 0x32, 0x03, 0x04, 0x15, 0x06, 0x07]);
        assert_eq!(params.p1, (0x02 << 16) | (0x03 << 8) | 0x04);
        assert_eq!(params.p2, (0x05 << 16) | (0x06 << 8) | 0x07);
        assert_eq!(params.p3, (0x1 << 16) | 0x01);
        assert_eq!(params.r_div, 3);
    }

    #[test]
    fn integer_multisynth_matches_c_float_math() {
        // MS6/MS7 use float (not double) math in C:
        // (800.0f / parms) / div_lut[r_div].
        assert!((ms_int_output_mhz(40, 0) - 20.0).abs() < 1e-6);
        assert!((ms_int_output_mhz(40, 1) - 10.0).abs() < 1e-6);
        // div_lut is the power-of-two table {1,2,4,...,128}.
        assert_eq!(DIV_LUT, [1, 2, 4, 8, 16, 32, 64, 128]);
    }
}
