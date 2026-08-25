//! Shared helpers for the airspy-tools CLI binaries.
//!
//! The binaries themselves (`airspy_info`, `airspy_rx`, …) are added as
//! `[[bin]]` targets while the port progresses; this library target
//! holds the argument-parsing and output-formatting code they share.
#![warn(missing_docs)]

pub mod flash_cli;
pub mod gpio_cli;
pub mod reg_cli;
pub mod rx;
pub mod rx_cli;
pub mod wav;

/// Failure to parse a `parse_u64`-style argument
/// (C returns `AIRSPY_ERROR_INVALID_PARAM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseU64Error;

impl std::fmt::Display for ParseU64Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AIRSPY_ERROR_INVALID_PARAM (-2)")
    }
}

impl std::error::Error for ParseU64Error {}

/// Parse a numeric CLI argument exactly as `parse_u64` in the C tools
/// does: a `0x`/`0X` prefix on the raw string selects base 16 and
/// `0b`/`0B` selects base 2 — but only when the string is longer than
/// two characters — otherwise base 10. The tail then goes through
/// `strtoull` semantics, and the whole string must be consumed
/// (`(s != s_end) && (*s_end == 0)`).
pub fn parse_u64(s: &str) -> Result<u64, ParseU64Error> {
    // Prefix detection inspects raw bytes before any whitespace
    // handling, exactly as the C code indexes s[0]/s[1]; both prefix
    // bytes are ASCII so slicing at 2 is boundary-safe when matched.
    let bytes = s.as_bytes();
    let (tail, base) = if s.len() > 2 && bytes[0] == b'0' {
        match bytes[1] {
            b'x' | b'X' => (&s[2..], 16),
            b'b' | b'B' => (&s[2..], 2),
            _ => (s, 10),
        }
    } else {
        (s, 10)
    };
    strtoull_whole(tail, base).ok_or(ParseU64Error)
}

/// `strtoull(s, &end, base)` with C's `parse_u64` acceptance check
/// (`s != s_end && *s_end == 0`): skip `isspace()` whitespace, accept
/// one optional sign (`-` negates with unsigned wraparound), in base
/// 16 strip one more `0x`/`0X` when a hex digit follows, consume
/// digits (saturating to `u64::MAX` on overflow, as strtoull clamps to
/// `ULLONG_MAX`) — and require that digits were consumed and nothing
/// remains.
fn strtoull_whole(s: &str, base: u32) -> Option<u64> {
    // C isspace(): space, \t, \n, \v, \f, \r.
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0B', '\x0C', '\r']);
    let (negative, s) = match s.strip_prefix(['+', '-']) {
        Some(rest) => (s.starts_with('-'), rest),
        None => (false, s),
    };
    let s = if base == 16 {
        match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_hexdigit()) => rest,
            _ => s,
        }
    } else {
        s
    };
    let mut value: u64 = 0;
    let mut saturated = false;
    let mut digits = 0usize;
    let mut chars = s.chars();
    for c in chars.by_ref() {
        let Some(d) = c.to_digit(base) else {
            // A non-digit before the end means the whole string was
            // not consumed — C's *s_end != 0 reject path.
            return None;
        };
        digits += 1;
        if !saturated {
            match value
                .checked_mul(u64::from(base))
                .and_then(|v| v.checked_add(u64::from(d)))
            {
                Some(v) => value = v,
                None => saturated = true,
            }
        }
    }
    if digits == 0 {
        return None;
    }
    // On overflow strtoull returns ULLONG_MAX without applying the
    // sign; otherwise '-' negates with unsigned wraparound.
    Some(if saturated {
        u64::MAX
    } else if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

/// The shared `-s serial_number_64bits` argument every tool's C
/// getopt table carries; callers needing a long form (only
/// `airspy_si5351c.c` lists `--serial`) add it.
pub fn serial_arg() -> clap::Arg {
    clap::Arg::new("serial")
        .short('s')
        .value_name("serial_number_64bits")
        .value_parser(parse_u64)
        .help("Open board with specified 64bits serial number")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_hex_and_binary_like_c_parse_u64() {
        // parse_u64 in airspy_info.c (and the other C tools): a 0x/0X
        // prefix selects base 16, 0b/0B selects base 2, otherwise
        // base 10 — and the whole string must be consumed.
        assert_eq!(parse_u64("644064DC"), Err(ParseU64Error));
        assert_eq!(parse_u64("12345"), Ok(12345));
        assert_eq!(parse_u64("0x644064DC"), Ok(0x6440_64DC));
        assert_eq!(parse_u64("0X644064DC"), Ok(0x6440_64DC));
        assert_eq!(parse_u64("0b1010"), Ok(10));
        assert_eq!(parse_u64("0B1010"), Ok(10));
    }

    #[test]
    fn requires_full_string_consumption() {
        // C checks (s != s_end) && (*s_end == 0).
        assert_eq!(parse_u64("123abc"), Err(ParseU64Error));
        assert_eq!(parse_u64("0x12ZZ"), Err(ParseU64Error));
        assert_eq!(parse_u64(""), Err(ParseU64Error));
        assert_eq!(parse_u64("0x"), Err(ParseU64Error));
    }

    #[test]
    fn inherits_strtoull_semantics_after_prefix_detection() {
        // C's parse_u64 detects the base prefix on the raw string,
        // then hands the tail to strtoull — which itself skips leading
        // whitespace, accepts one sign ('-' wraps as unsigned),
        // saturates on overflow, and in base 16 strips another 0x.
        assert_eq!(parse_u64(" 10"), Ok(10));
        assert_eq!(parse_u64("+10"), Ok(10));
        assert_eq!(parse_u64("-1"), Ok(u64::MAX));
        assert_eq!(parse_u64("99999999999999999999999"), Ok(u64::MAX));
        assert_eq!(parse_u64("0x0x10"), Ok(0x10));
        // Whitespace inside the prefix defeats detection: base stays
        // 10 and strtoull stops at 'x'.
        assert_eq!(parse_u64(" 0x10"), Err(ParseU64Error));
    }

    #[test]
    fn short_strings_skip_prefix_detection() {
        // C only inspects prefixes when strlen(s) > 2, so "0x" alone
        // is a base-10 parse that fails at 'x', and "0b" likewise.
        assert_eq!(parse_u64("0b"), Err(ParseU64Error));
        // Two-char decimals still parse.
        assert_eq!(parse_u64("42"), Ok(42));
    }
}
