//! Shared engine for the `airspy_gpio` and `airspy_gpiodir` binaries,
//! ported from `airspy-tools/src/airspy_gpio.c` and
//! `airspy_gpiodir.c` (near-identical twins): C-semantics `parse_u8`,
//! the port/pin tables, and the order-sensitive option replay of the
//! C getopt loops. The device-facing print formats stay in the
//! binaries; everything here is hardware-free and unit-tested.

use libairspy_rs::Error;
use libairspy_rs::commands::{GpioPin, GpioPort};

/// `PORT_NUM_MIN` in `airspy_gpio.c` / `airspy_gpiodir.c`.
pub const PORT_NUM_MIN: u8 = 0;
/// `PORT_NUM_MAX` in `airspy_gpio.c` / `airspy_gpiodir.c`.
pub const PORT_NUM_MAX: u8 = 7;
/// `PIN_NUM_MIN` in `airspy_gpio.c` / `airspy_gpiodir.c`.
pub const PIN_NUM_MIN: u8 = 0;
/// `PIN_NUM_MAX` in `airspy_gpio.c` / `airspy_gpiodir.c`.
pub const PIN_NUM_MAX: u8 = 31;

/// The ports in C iteration order (`GPIO_PORT0..GPIO_PORT7`, the
/// `dump_ports` loop in `airspy_gpio.c` / `airspy_gpiodir.c`); index
/// = port number, length = `PORT_NUM_MAX + 1`.
pub const GPIO_PORTS: [GpioPort; PORT_NUM_MAX as usize + 1] = [
    GpioPort::Port0,
    GpioPort::Port1,
    GpioPort::Port2,
    GpioPort::Port3,
    GpioPort::Port4,
    GpioPort::Port5,
    GpioPort::Port6,
    GpioPort::Port7,
];

/// The pins in C iteration order (`GPIO_PIN0..GPIO_PIN31`, the
/// `dump_port` loop in `airspy_gpio.c` / `airspy_gpiodir.c`); index =
/// pin number, length = `PIN_NUM_MAX + 1`.
pub const GPIO_PINS: [GpioPin; PIN_NUM_MAX as usize + 1] = [
    GpioPin::Pin0,
    GpioPin::Pin1,
    GpioPin::Pin2,
    GpioPin::Pin3,
    GpioPin::Pin4,
    GpioPin::Pin5,
    GpioPin::Pin6,
    GpioPin::Pin7,
    GpioPin::Pin8,
    GpioPin::Pin9,
    GpioPin::Pin10,
    GpioPin::Pin11,
    GpioPin::Pin12,
    GpioPin::Pin13,
    GpioPin::Pin14,
    GpioPin::Pin15,
    GpioPin::Pin16,
    GpioPin::Pin17,
    GpioPin::Pin18,
    GpioPin::Pin19,
    GpioPin::Pin20,
    GpioPin::Pin21,
    GpioPin::Pin22,
    GpioPin::Pin23,
    GpioPin::Pin24,
    GpioPin::Pin25,
    GpioPin::Pin26,
    GpioPin::Pin27,
    GpioPin::Pin28,
    GpioPin::Pin29,
    GpioPin::Pin30,
    GpioPin::Pin31,
];

/// `parse_u8` in `airspy_gpio.c` / `airspy_gpiodir.c`: `strtol` base
/// 10 (leading `isspace()` whitespace and one optional sign), the
/// whole string consumed (`s != s_end && *s_end == 0`), then
/// `0 <= value < 256`. `None` is C's `AIRSPY_ERROR_INVALID_PARAM`.
pub fn parse_u8(s: &str) -> Option<u8> {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0B', '\x0C', '\r']);
    let (negative, digits) = match s.strip_prefix(['+', '-']) {
        Some(rest) => (s.starts_with('-'), rest),
        None => (false, s),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars() {
        // A non-digit is C's *s_end != 0 reject path.
        let d = c.to_digit(10)?;
        // strtol clamps to LONG_MAX on overflow — saturating keeps
        // the value out of range just the same.
        value = value.saturating_mul(10).saturating_add(i64::from(d));
    }
    if negative {
        value = -value;
    }
    u8::try_from(value).ok()
}

/// One replayed occurrence of the C getopt loop's options, in
/// command-line order. Values stay raw strings — C parses each at
/// its own loop iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpioOp {
    /// `-p <port>` — latch the port for later operations.
    Port(String),
    /// `-n <pin>` — latch the pin for later operations.
    Pin(String),
    /// `-r` — dump using the currently latched selection.
    Read,
    /// `-w <value>` — write to the currently latched selection.
    Write(String),
}

/// What a `-r` acts on, from the C sentinel rules: no port latched →
/// everything (a latched pin is ignored); port only → the whole
/// port; port and pin → the single pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadScope {
    /// `dump_ports` — all pins on all ports.
    All,
    /// `dump_port` — all pins on one port.
    Port(GpioPort),
    /// `dump_port_pin` — one pin.
    PortPin(GpioPort, GpioPin),
}

/// How the replay ended, mirroring C `main()`'s `result` variable:
/// still `AIRSPY_ERROR_OTHER` when no option ran at all (usage
/// prints, exit stays 0), success (any option — including a bare
/// `-p`/`-n` — succeeded last), or a failure that stopped the loop
/// (usage prints; deviation: the binaries exit nonzero).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// No operation was attempted.
    NoOps,
    /// Every operation succeeded.
    Completed,
    /// An operation failed and stopped the replay.
    Failed,
}

/// Run C's `dump_ports` / `dump_port` / `dump_port_pin` loop
/// semantics over `per_pin`: a full-port dump visits all 32 pins and
/// returns the LAST pin's result; the all-ports dump stops after a
/// port whose final result failed.
pub fn dump_scope(
    scope: ReadScope,
    per_pin: &mut impl FnMut(GpioPort, GpioPin) -> Result<(), Error>,
) -> Result<(), Error> {
    match scope {
        ReadScope::PortPin(port, pin) => per_pin(port, pin),
        ReadScope::Port(port) => {
            let mut result = Ok(());
            for pin in GPIO_PINS {
                result = per_pin(port, pin);
            }
            result
        }
        ReadScope::All => {
            for port in GPIO_PORTS {
                dump_scope(ReadScope::Port(port), per_pin)?;
            }
            Ok(())
        }
    }
}

/// Replay the C getopt loop: `-p`/`-n` latch state, `-r`/`-w` act on
/// it immediately, and the first failure stops the loop. `read` and
/// `write` do the device work (and their own success/failure prints);
/// `out` receives the loop's own messages — the C range errors and
/// the `argument error: %s (%d)` line printed after a failed
/// operation.
///
/// Deviation: C passes its 255 "unset" sentinel straight to
/// `airspy_gpio_write`, putting a garbage `port_pin` on the wire; a
/// `-w` without a latched port and pin is an error here.
pub fn replay_ops(
    ops: &[GpioOp],
    read: &mut impl FnMut(ReadScope) -> Result<(), Error>,
    write: &mut impl FnMut(GpioPort, GpioPin, u8) -> Result<(), Error>,
    out: &mut impl FnMut(String),
) -> ReplayOutcome {
    let mut port: Option<GpioPort> = None;
    let mut pin: Option<GpioPin> = None;
    let mut outcome = ReplayOutcome::NoOps;
    for op in ops {
        let ok = match op {
            GpioOp::Port(raw) => latch(raw, &GPIO_PORTS, &mut port, &mut outcome, out, "-p"),
            GpioOp::Pin(raw) => latch(raw, &GPIO_PINS, &mut pin, &mut outcome, out, "-n"),
            GpioOp::Read => {
                let scope = match (port, pin) {
                    (None, _) => ReadScope::All,
                    (Some(port), None) => ReadScope::Port(port),
                    (Some(port), Some(pin)) => ReadScope::PortPin(port, pin),
                };
                run_operation(read(scope), &mut outcome, out)
            }
            GpioOp::Write(raw) => match (parse_u8(raw), port, pin) {
                (Some(value), Some(port), Some(pin)) => {
                    run_operation(write(port, pin, value), &mut outcome, out)
                }
                (None, _, _) => false,
                _ => {
                    out("error: -w requires -p and -n".into());
                    false
                }
            },
        };
        if !ok {
            return ReplayOutcome::Failed;
        }
    }
    outcome
}

/// The shared body of C's `-p`/`-n` cases: parse, range-check
/// against the table, latch on success — and store `parse_u8`'s
/// SUCCESS in the outcome, so a bare `-p` suppresses `usage()` just
/// as C's `result` variable does. The range error prints C's exact
/// message.
fn latch<T: Copy>(
    raw: &str,
    table: &[T],
    slot: &mut Option<T>,
    outcome: &mut ReplayOutcome,
    out: &mut impl FnMut(String),
    flag: &str,
) -> bool {
    if let Some(value) = parse_u8(raw).and_then(|v| table.get(usize::from(v))) {
        *slot = Some(*value);
        *outcome = ReplayOutcome::Completed;
        true
    } else {
        let max = table.len() - 1;
        out(format!(
            "Error parameter {flag} shall be between 0 and {max}"
        ));
        false
    }
}

/// The shared serial-number print and device open from both C tools'
/// `main()`: the `-s` first-pass print, `airspy_open_sn` vs
/// `airspy_open`, and the C failure message (stdout) — the caller
/// prints its usage and exits on `None` (the message has already
/// been printed here, C-style).
pub fn open_from_matches(matches: &clap::ArgMatches) -> Option<libairspy_rs::Device> {
    let serial = matches.get_one::<u64>("serial").copied();
    if let Some(serial) = serial {
        println!(
            "Board serial number to open: 0x{:08X}{:08X}",
            (serial >> 32) as u32,
            (serial & 0xFFFF_FFFF) as u32
        );
    }
    let (result, context) = match serial {
        Some(serial) => (
            libairspy_rs::Device::open_serial(serial),
            "airspy_open_sn()",
        ),
        None => (libairspy_rs::Device::open(), "airspy_open()"),
    };
    match result {
        Ok(device) => Some(device),
        Err(err) => {
            println!("{context} failed: {} ({})", err.name(), err.code());
            None
        }
    }
}

/// The shared tail of C's `-r`/`-w` cases: on failure print
/// `argument error: %s (%d)` and stop; on success mark progress.
fn run_operation(
    result: Result<(), Error>,
    outcome: &mut ReplayOutcome,
    out: &mut impl FnMut(String),
) -> bool {
    match result {
        Ok(()) => {
            *outcome = ReplayOutcome::Completed;
            true
        }
        Err(err) => {
            out(format!("argument error: {} ({})", err.name(), err.code()));
            false
        }
    }
}

/// The shared clap command for both GPIO tools — the C
/// `getopt_long(argc, argv, "p:n:rw:s:", long_options, ...)` option
/// table. Every operation option repeats, and the original
/// command-line order is recovered with [`ops_from_matches`].
pub fn gpio_command(name: &'static str, about: &'static str) -> clap::Command {
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
            clap::Arg::new("port")
                .short('p')
                .long("port_no")
                .value_name("p")
                .action(clap::ArgAction::Append)
                .help("set port number<p>[0,7] for subsequent read/write operations"),
        )
        .arg(
            clap::Arg::new("pin")
                .short('n')
                .long("pin_no")
                .value_name("n")
                .action(clap::ArgAction::Append)
                .help("set pin number<n>[0,31] for subsequent read/write operations"),
        )
        .arg(
            // Append with zero values so every -r occurrence records
            // its own index (Count keeps only a total).
            clap::Arg::new("read")
                .short('r')
                .long("read")
                .action(clap::ArgAction::Append)
                .num_args(0)
                .default_missing_value("r")
                .help("read the selection set by the last -p/-n, or all ports/pins"),
        )
        .arg(
            clap::Arg::new("write")
                .short('w')
                .long("write")
                .value_name("v")
                .action(clap::ArgAction::Append)
                .help("write the selection set by the last -p/-n with value<v>[0,1]"),
        )
        .arg(crate::serial_arg())
}

/// Recover the C getopt loop's op sequence: each occurrence of
/// `-p`/`-n`/`-r`/`-w` in original command-line order (clap argument
/// indices), so later `-p`/`-n` retarget later operations exactly as
/// in C.
pub fn ops_from_matches(matches: &clap::ArgMatches) -> Vec<GpioOp> {
    let mut ops: Vec<(usize, GpioOp)> = Vec::new();
    for (id, make) in [
        ("port", GpioOp::Port as fn(String) -> GpioOp),
        ("pin", GpioOp::Pin as fn(String) -> GpioOp),
        ("write", GpioOp::Write as fn(String) -> GpioOp),
    ] {
        if let (Some(values), Some(indices)) =
            (matches.get_many::<String>(id), matches.indices_of(id))
        {
            ops.extend(indices.zip(values).map(|(i, v)| (i, make(v.clone()))));
        }
    }
    if let Some(indices) = matches.indices_of("read") {
        ops.extend(indices.map(|i| (i, GpioOp::Read)));
    }
    ops.sort_by_key(|(i, _)| *i);
    ops.into_iter().map(|(_, op)| op).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libairspy_rs::Error;
    use libairspy_rs::commands::{GpioPin, GpioPort};

    #[test]
    fn ops_recover_command_line_order() {
        // The imperative C loop depends on interleaving: -p 0 -r -p 1
        // -n 2 -w 1 must replay in exactly that order.
        let matches = gpio_command("t", "t")
            .try_get_matches_from(["t", "-p", "0", "-r", "-p", "1", "-n", "2", "-w", "1", "-r"])
            .expect("parse");
        assert_eq!(
            ops_from_matches(&matches),
            [
                GpioOp::Port("0".into()),
                GpioOp::Read,
                GpioOp::Port("1".into()),
                GpioOp::Pin("2".into()),
                GpioOp::Write("1".into()),
                GpioOp::Read,
            ]
        );
        let none = gpio_command("t", "t")
            .try_get_matches_from(["t"])
            .expect("parse");
        assert!(ops_from_matches(&none).is_empty());
    }

    #[test]
    fn constants_match_c_defines() {
        assert_eq!(PORT_NUM_MIN, 0);
        assert_eq!(PORT_NUM_MAX, 7);
        assert_eq!(PIN_NUM_MIN, 0);
        assert_eq!(PIN_NUM_MAX, 31);
        assert_eq!(GPIO_PORTS.len(), 8);
        assert_eq!(GPIO_PINS.len(), 32);
        assert_eq!(GPIO_PORTS[7], GpioPort::Port7);
        assert_eq!(GPIO_PINS[31], GpioPin::Pin31);
    }

    #[test]
    fn parse_u8_follows_c_strtol_semantics() {
        // parse_u8 in airspy_gpio.c: strtol base 10, full consumption
        // (s != s_end && *s_end == 0), then 0 <= v < 256.
        assert_eq!(parse_u8("0"), Some(0));
        assert_eq!(parse_u8("255"), Some(255));
        assert_eq!(parse_u8(" 12"), Some(12));
        assert_eq!(parse_u8("+7"), Some(7));
        assert_eq!(parse_u8("256"), None);
        assert_eq!(parse_u8("-1"), None);
        assert_eq!(parse_u8("0x10"), None);
        assert_eq!(parse_u8("12abc"), None);
        assert_eq!(parse_u8(""), None);
        assert_eq!(parse_u8("99999999999999999999"), None);
    }

    /// Recording harness: log every read scope and write.
    fn run(ops: &[GpioOp]) -> (ReplayOutcome, Vec<String>, Vec<String>) {
        let log = std::cell::RefCell::new(Vec::new());
        let mut out = Vec::new();
        let outcome = replay_ops(
            ops,
            &mut |scope| {
                log.borrow_mut().push(format!("read {scope:?}"));
                Ok(())
            },
            &mut |port, pin, value| {
                log.borrow_mut()
                    .push(format!("write {port:?} {pin:?} {value}"));
                Ok(())
            },
            &mut |line| out.push(line),
        );
        (outcome, log.into_inner(), out)
    }

    #[test]
    fn read_scopes_follow_c_sentinel_rules() {
        // C: -r with no -p dumps everything (a set -n is ignored);
        // -p alone dumps the port; -p and -n dump the single pin.
        let (o, log, _) = run(&[GpioOp::Read]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert_eq!(log, ["read All"]);

        let (_, log, _) = run(&[GpioOp::Pin("3".into()), GpioOp::Read]);
        assert_eq!(log, ["read All"]);

        let (_, log, _) = run(&[GpioOp::Port("2".into()), GpioOp::Read]);
        assert_eq!(log, ["read Port(Port2)"]);

        let (_, log, _) = run(&[
            GpioOp::Port("0".into()),
            GpioOp::Pin("12".into()),
            GpioOp::Read,
        ]);
        assert_eq!(log, ["read PortPin(Port0, Pin12)"]);
    }

    #[test]
    fn ops_replay_in_command_line_order() {
        // The C getopt loop is imperative: later -p/-n change what a
        // later -r/-w acts on.
        let (o, log, _) = run(&[
            GpioOp::Port("0".into()),
            GpioOp::Pin("12".into()),
            GpioOp::Read,
            GpioOp::Pin("13".into()),
            GpioOp::Write("1".into()),
        ]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert_eq!(log, ["read PortPin(Port0, Pin12)", "write Port0 Pin13 1"]);
    }

    #[test]
    fn range_errors_print_c_messages_and_stop() {
        let (o, log, out) = run(&[GpioOp::Port("8".into()), GpioOp::Read]);
        assert_eq!(o, ReplayOutcome::Failed);
        assert!(log.is_empty());
        assert_eq!(out, ["Error parameter -p shall be between 0 and 7"]);

        let (o, _, out) = run(&[GpioOp::Pin("32".into())]);
        assert_eq!(o, ReplayOutcome::Failed);
        assert_eq!(out, ["Error parameter -n shall be between 0 and 31"]);
    }

    #[test]
    fn write_requires_port_and_pin() {
        // Deviation: C passes the 255 sentinel straight to
        // airspy_gpio_write, sending a garbage port_pin on the wire.
        let (o, log, out) = run(&[GpioOp::Write("1".into())]);
        assert_eq!(o, ReplayOutcome::Failed);
        assert!(log.is_empty());
        assert_eq!(out, ["error: -w requires -p and -n"]);
    }

    #[test]
    fn write_value_passes_through_like_c() {
        // C's usage says [0,1] but parse_u8 accepts any byte and the
        // library forwards it; faithful.
        let (o, log, _) = run(&[
            GpioOp::Port("1".into()),
            GpioOp::Pin("7".into()),
            GpioOp::Write("255".into()),
        ]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert_eq!(log, ["write Port1 Pin7 255"]);
    }

    #[test]
    fn unparseable_write_value_fails_silently_like_c() {
        // C's case 'w': parse_u8 returns INVALID_PARAM, no message
        // prints in that arm — the loop just breaks and usage()
        // follows. No write reaches the device.
        let (o, log, out) = run(&[
            GpioOp::Port("0".into()),
            GpioOp::Pin("1".into()),
            GpioOp::Write("abc".into()),
        ]);
        assert_eq!(o, ReplayOutcome::Failed);
        assert!(log.is_empty());
        assert!(out.is_empty());
    }

    #[test]
    fn no_ops_reports_noops_outcome() {
        // C: result stays AIRSPY_ERROR_OTHER when no option ran, so
        // usage() prints after open/close and exit stays 0 — but a
        // successful -p/-n alone sets result to SUCCESS (case 'p'
        // stores parse_u8's return), so no usage prints for it.
        let (o, log, out) = run(&[]);
        assert_eq!(o, ReplayOutcome::NoOps);
        assert!(log.is_empty() && out.is_empty());

        let (o, log, _) = run(&[GpioOp::Port("0".into())]);
        assert_eq!(o, ReplayOutcome::Completed);
        assert!(log.is_empty());
    }

    #[test]
    fn failed_operation_prints_argument_error_and_stops() {
        // C main(): dump/write result != SUCCESS → "argument error:
        // %s (%d)" then the loop breaks.
        let mut out = Vec::new();
        let outcome = replay_ops(
            &[GpioOp::Read, GpioOp::Read],
            &mut |_| Err(Error::NotFound),
            &mut |_, _, _| Ok(()),
            &mut |line| out.push(line),
        );
        assert_eq!(outcome, ReplayOutcome::Failed);
        assert_eq!(out, ["argument error: AIRSPY_ERROR_NOT_FOUND (-5)"]);
    }

    #[test]
    fn dump_scope_mirrors_c_loop_semantics() {
        // dump_port visits all 32 pins even when one fails and
        // returns the LAST pin's result; dump_ports stops after a
        // port whose final result failed.
        let mut calls = Vec::new();
        let result = dump_scope(ReadScope::Port(GpioPort::Port1), &mut |port, pin| {
            calls.push((port, pin));
            if pin == GpioPin::Pin5 {
                Err(Error::NotFound)
            } else {
                Ok(())
            }
        });
        assert_eq!(calls.len(), 32);
        assert!(result.is_ok()); // pin 31 succeeded last

        let mut count = 0;
        let result = dump_scope(ReadScope::All, &mut |_, pin| {
            count += 1;
            if pin == GpioPin::Pin31 {
                Err(Error::NotFound)
            } else {
                Ok(())
            }
        });
        // Port0's last pin fails → dump_ports breaks after 32 calls.
        assert_eq!(count, 32);
        assert!(result.is_err());

        let mut single = Vec::new();
        let result = dump_scope(
            ReadScope::PortPin(GpioPort::Port3, GpioPin::Pin9),
            &mut |port, pin| {
                single.push((port, pin));
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(single, [(GpioPort::Port3, GpioPin::Pin9)]);
    }
}
