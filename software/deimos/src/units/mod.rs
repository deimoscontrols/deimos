//! UCUM-subset unit parser and dimensional comparison.
//!
//! Calcs and peripherals declare engineering units as strings (e.g., `"K"`,
//! `"m/s"`, `"kg.m/s^2"`). This module parses them into a `(Dimension, scale)`
//! tuple so the orchestrator can validate that wires connecting calcs carry
//! compatible units at controller-init time.
//!
//! The grammar is intentionally a bounded subset of UCUM:
//!
//! ```text
//! unit       := factor ( ('*' | '.' | '/') factor )*
//! factor     := base_unit ( '^' integer )?
//! base_unit  := letter+ | '1'    // dimensionless = "1"
//! ```
//!
//! Whitespace is ignored. Recognized base units and SI prefixes live on
//! hard-coded allow-lists below — adding a new unit is a one-line edit.
//!
//! ## Why in-tree (vs. `uom`)
//!
//! `uom` and similar typed-units crates would force every unit-bearing trait
//! method into a generic over `Quantity<...>`, which doesn't fit the existing
//! `Vec<Option<String>>` carrier in `ControllerCtx`. The string-with-parser
//! approach lets units stay opaque at the trait boundary while still catching
//! the typo case (`RtdPt100`(K) → `Affine`(declared input Pa)) at init time.

use std::fmt;

/// Dimensional vector in SI base units. Each field is the integer exponent
/// of that base dimension in the unit's expansion (e.g., `N` is mass=1,
/// length=1, time=−2; everything else 0). Stored as `i8` because real-world
/// physics doesn't go past `±127` and packing keeps `Dimension` cheap to
/// compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dimension {
    pub mass: i8,
    pub length: i8,
    pub time: i8,
    pub current: i8,
    pub temperature: i8,
    pub amount: i8,
    pub luminous: i8,
}

impl Dimension {
    pub const fn new(
        mass: i8,
        length: i8,
        time: i8,
        current: i8,
        temperature: i8,
        amount: i8,
        luminous: i8,
    ) -> Self {
        Self {
            mass,
            length,
            time,
            current,
            temperature,
            amount,
            luminous,
        }
    }

    /// Multiply two dimensions (i.e., add their exponents component-wise).
    fn mul(self, other: Self) -> Self {
        Self {
            mass: self.mass + other.mass,
            length: self.length + other.length,
            time: self.time + other.time,
            current: self.current + other.current,
            temperature: self.temperature + other.temperature,
            amount: self.amount + other.amount,
            luminous: self.luminous + other.luminous,
        }
    }

    /// Raise this dimension to an integer exponent (multiply each component).
    fn pow(self, exp: i8) -> Self {
        Self {
            mass: self.mass * exp,
            length: self.length * exp,
            time: self.time * exp,
            current: self.current * exp,
            temperature: self.temperature * exp,
            amount: self.amount * exp,
            luminous: self.luminous * exp,
        }
    }

    /// Render a short human-readable summary like `"temperature"` or
    /// `"mass·length·time^-2"`. Used by error messages so the operator can
    /// see *why* two units don't match without re-deriving it themselves.
    pub fn describe(&self) -> String {
        // Special cases for the common single-dimension units the operator
        // is most likely to recognize at a glance.
        if *self == Self::default() {
            return "dimensionless".to_string();
        }
        let single = [
            (self.mass, "mass"),
            (self.length, "length"),
            (self.time, "time"),
            (self.current, "current"),
            (self.temperature, "temperature"),
            (self.amount, "amount"),
            (self.luminous, "luminous"),
        ];
        let nonzero: Vec<_> = single.iter().filter(|(e, _)| *e != 0).collect();
        if nonzero.len() == 1 && nonzero[0].0 == 1 {
            return nonzero[0].1.to_string();
        }
        let mut parts = Vec::new();
        for (exp, name) in single {
            if exp == 0 {
                continue;
            } else if exp == 1 {
                parts.push(name.to_string());
            } else {
                parts.push(format!("{name}^{exp}"));
            }
        }
        parts.join("·")
    }
}

/// A parsed unit string: its dimensional vector plus a multiplicative scale
/// factor relative to the SI base for that dimension. For example, `kPa`
/// parses to `dim = mass^1·length^-1·time^-2`, `scale = 1000.0`.
///
/// Temperature scales with offsets (`°C`, `°F`) cannot be represented
/// as a single scale factor — they need an additive offset too. v1 only
/// supports `K` and `°C`/`degC`, and treats `°C` as having a *different*
/// `scale` than `K` so `units_equal` reports them as unequal. The
/// orchestrator's pass-1 mismatch handler then attaches the K↔°C-specific
/// `Affine` hint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedUnit {
    pub dim: Dimension,
    pub scale: f64,
}

/// Error returned by `parse_unit` when the input string isn't recognized.
/// Always echoes the offending string verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub input: String,
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not parse unit string {:?}: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for ParseError {}

/// Hard-coded base units. Each entry is `(name, Dimension, scale)`.
///
/// `°C` and `degC` carry the temperature dimension with a *non-1* scale so
/// equality with `K` fails — the orchestrator catches the K↔°C case as a
/// mismatch and attaches a hint. We pick `1.0e9` as an arbitrary marker.
///
/// `1.0e9` numerically coincides with the `G` (giga) prefix scale on `Pa`,
/// `Hz`, `W`, `J`, `N`, etc. That collision is harmless: temperature is the
/// only dimension where this scale appears, no SI prefixes are allowed on
/// `K`/`°C`/`degC` (see `prefix_allowed`), and `unit_mismatch_message`'s
/// affine-hint path gates on temperature dimension before consulting the
/// scale. So `(temperature=1, scale=1e9)` is a unique identifier for
/// `°C`/`degC` even though `1e9` itself is not unique across all units.
///
/// Encoding a true offset would require a wider `ParsedUnit` and isn't
/// needed for v1's fail-fast contract.
const TEMP_C_SCALE: f64 = 1.0e9;

/// Lookup result for a base unit (name, dimensional vector, scale relative
/// to the SI base).
struct BaseUnit {
    name: &'static str,
    dim: Dimension,
    scale: f64,
}

const BASE_UNITS: &[BaseUnit] = &[
    // Dimensionless
    BaseUnit {
        name: "1",
        dim: Dimension::new(0, 0, 0, 0, 0, 0, 0),
        scale: 1.0,
    },
    // SI base
    BaseUnit {
        name: "s",
        dim: Dimension::new(0, 0, 1, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        name: "m",
        dim: Dimension::new(0, 1, 0, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        name: "kg",
        dim: Dimension::new(1, 0, 0, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        name: "A",
        dim: Dimension::new(0, 0, 0, 1, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        name: "K",
        dim: Dimension::new(0, 0, 0, 0, 1, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        name: "mol",
        dim: Dimension::new(0, 0, 0, 0, 0, 1, 0),
        scale: 1.0,
    },
    BaseUnit {
        name: "cd",
        dim: Dimension::new(0, 0, 0, 0, 0, 0, 1),
        scale: 1.0,
    },
    // Derived SI
    BaseUnit {
        // Hz = 1/s
        name: "Hz",
        dim: Dimension::new(0, 0, -1, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // N = kg·m/s²
        name: "N",
        dim: Dimension::new(1, 1, -2, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // Pa = kg/(m·s²)
        name: "Pa",
        dim: Dimension::new(1, -1, -2, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // J = kg·m²/s²
        name: "J",
        dim: Dimension::new(1, 2, -2, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // W = kg·m²/s³
        name: "W",
        dim: Dimension::new(1, 2, -3, 0, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // C (charge) = A·s. Note the disambiguation from °C below.
        name: "C",
        dim: Dimension::new(0, 0, 1, 1, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // V = kg·m²/(A·s³)
        name: "V",
        dim: Dimension::new(1, 2, -3, -1, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // ohm = kg·m²/(A²·s³)
        name: "ohm",
        dim: Dimension::new(1, 2, -3, -2, 0, 0, 0),
        scale: 1.0,
    },
    BaseUnit {
        // rad — dimensionless angle. `scale=1.0` means `rad` is equal to `1`
        // under `units_equal` (which compares dim and scale), matching the
        // physics (radians are dimensionless) and UCUM.
        name: "rad",
        dim: Dimension::new(0, 0, 0, 0, 0, 0, 0),
        scale: 1.0,
    },
    // Temperature aliases for Celsius. Same dimension as K but a different
    // scale, so equality with K fails and the orchestrator catches the
    // mismatch.
    BaseUnit {
        name: "°C",
        dim: Dimension::new(0, 0, 0, 0, 1, 0, 0),
        scale: TEMP_C_SCALE,
    },
    BaseUnit {
        name: "degC",
        dim: Dimension::new(0, 0, 0, 0, 1, 0, 0),
        scale: TEMP_C_SCALE,
    },
];

/// Look up a base unit by its exact name. `None` if not recognized.
fn lookup_base(name: &str) -> Option<&'static BaseUnit> {
    BASE_UNITS.iter().find(|b| b.name == name)
}

/// SI-prefix allow-list per base unit. Each entry maps a prefix character
/// (e.g., `'k'` for kilo) to its multiplier. The allow-list is per-base
/// because some prefix+base combinations are ambiguous (`mK` looks like
/// milli-Kelvin but `m` is also the meter base unit; to avoid the
/// ambiguity, we reject all prefixes on `K`).
//
// Prefixes are also rejected on `m` (meter), `mol`, `cd`, `°C`,
// `degC`, `rad`, and `1` for the same kind of ambiguity reason — `mm`
// would be milli-meter but the parser sees `m` then `m` and we'd have to
// special-case longest-match. Operators almost never type `mm` etc. into
// a controller config; if they need to, they can write the SI base.
//
// Allow-listed bases for SI prefixes are the common pressure cases
// (`kPa`, `MPa`, `mPa`) plus the few others where prefixes
// are unambiguous and common in instrumentation (`mV`, `mA`, `kHz`, `MHz`,
// `mN`, `kN`, `mW`, `kW`, `mJ`, `kJ`, `ms`, `ns`, `us`, `µs`, `kohm`, `Mohm`).
struct PrefixSet {
    base: &'static str,
    allowed: &'static [(char, f64)],
}

const PREFIXES: &[PrefixSet] = &[
    PrefixSet {
        base: "Pa",
        allowed: &[('m', 1e-3), ('k', 1e3), ('M', 1e6), ('G', 1e9)],
    },
    PrefixSet {
        base: "Hz",
        allowed: &[('m', 1e-3), ('k', 1e3), ('M', 1e6), ('G', 1e9)],
    },
    PrefixSet {
        base: "V",
        allowed: &[('m', 1e-3), ('u', 1e-6), ('µ', 1e-6), ('k', 1e3)],
    },
    PrefixSet {
        base: "A",
        allowed: &[('m', 1e-3), ('u', 1e-6), ('µ', 1e-6), ('k', 1e3)],
    },
    PrefixSet {
        base: "N",
        allowed: &[('m', 1e-3), ('k', 1e3), ('M', 1e6)],
    },
    PrefixSet {
        base: "W",
        allowed: &[('m', 1e-3), ('k', 1e3), ('M', 1e6)],
    },
    PrefixSet {
        base: "J",
        allowed: &[('m', 1e-3), ('k', 1e3), ('M', 1e6)],
    },
    PrefixSet {
        base: "s",
        allowed: &[('m', 1e-3), ('u', 1e-6), ('µ', 1e-6), ('n', 1e-9)],
    },
    PrefixSet {
        base: "ohm",
        allowed: &[('k', 1e3), ('M', 1e6)],
    },
    PrefixSet {
        base: "C",
        // Coulomb (charge) — small charges in mC, µC. Note: this is the
        // *charge* `C`, not °C / degC (those have no prefixes by design).
        allowed: &[('m', 1e-3), ('u', 1e-6), ('µ', 1e-6)],
    },
];

/// If `name` looks like `<prefix><base>` where the prefix is on the base's
/// allow-list, return `(scale, base)`. Else `None`. Tries every base on
/// the prefix list and returns the *first* match — bases here are unique,
/// so order doesn't matter for correctness.
fn try_prefixed(name: &str) -> Option<(f64, &'static BaseUnit)> {
    for set in PREFIXES {
        let base = set.base;
        if let Some(rest) = name.strip_suffix(base) {
            // `rest` must be exactly one prefix char. We compare by char,
            // not by byte, because `µ` is a multi-byte UTF-8 character.
            let mut chars = rest.chars();
            let prefix = chars.next()?;
            if chars.next().is_some() {
                continue;
            }
            for (allowed_prefix, scale) in set.allowed {
                if *allowed_prefix == prefix {
                    let base_unit = lookup_base(base)?;
                    return Some((*scale, base_unit));
                }
            }
        }
    }
    None
}

/// Resolve a single token (e.g., `"kPa"`, `"K"`, `"furlongs"`) to a
/// `(dim, scale)` pair. Tries an exact-base lookup first, then a
/// prefix+base lookup.
fn resolve_token(token: &str, original: &str) -> Result<(Dimension, f64), ParseError> {
    if let Some(base) = lookup_base(token) {
        return Ok((base.dim, base.scale));
    }
    if let Some((prefix_scale, base)) = try_prefixed(token) {
        return Ok((base.dim, base.scale * prefix_scale));
    }
    Err(ParseError {
        input: original.to_string(),
        reason: format!("unknown unit token {token:?}"),
    })
}

/// Parse a unit string into its dimension and scale. Whitespace is
/// stripped before parsing; the original is preserved for error messages.
///
/// Grammar (UCUM subset):
///
/// ```text
/// unit       := factor ( ('*' | '.' | '/') factor )*
/// factor     := token ( '^' integer )?
/// token      := ascii letters or digits, e.g., kPa, m, mol, K, °C, degC, 1
/// ```
///
/// Parens are not supported in v1 — every operator-typed unit string in
/// the codebase today is flat. If we need them, this is the place to add
/// them.
pub fn parse_unit(input: &str) -> Result<ParsedUnit, ParseError> {
    let stripped: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if stripped.is_empty() {
        return Err(ParseError {
            input: input.to_string(),
            reason: "empty unit string".to_string(),
        });
    }

    // Walk through `stripped` accumulating tokens. Each token is preceded
    // by an operator (`*`, `.`, or `/`); we treat the implicit leading
    // operator as `*`. After each token we look for an optional `^integer`.
    let mut dim = Dimension::default();
    let mut scale = 1.0_f64;
    let mut op = '*'; // current operator: applies to the next token
    let mut chars = stripped.chars().peekable();

    while chars.peek().is_some() {
        // Read one token: contiguous letters / digits / `°` (for °C).
        // Operators terminate the token.
        let mut token = String::new();
        while let Some(&c) = chars.peek() {
            if c == '*' || c == '.' || c == '/' || c == '^' {
                break;
            }
            token.push(c);
            chars.next();
        }
        if token.is_empty() {
            return Err(ParseError {
                input: input.to_string(),
                reason: format!("expected a unit token after operator '{op}'"),
            });
        }

        // Optional `^integer` exponent.
        let mut exp: i8 = 1;
        if chars.peek() == Some(&'^') {
            chars.next();
            let mut exp_str = String::new();
            // Allow a leading sign on the exponent (e.g., `m^-1`).
            if matches!(chars.peek(), Some(&'-') | Some(&'+')) {
                exp_str.push(chars.next().unwrap());
            }
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    exp_str.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if exp_str.is_empty() || exp_str == "-" || exp_str == "+" {
                return Err(ParseError {
                    input: input.to_string(),
                    reason: format!("expected integer exponent after '^' in token {token:?}"),
                });
            }
            exp = exp_str.parse::<i8>().map_err(|_| ParseError {
                input: input.to_string(),
                reason: format!("exponent {exp_str:?} out of range for token {token:?}"),
            })?;
        }

        // Resolve the token to a dimension+scale and apply op+exp.
        let (token_dim, token_scale) = resolve_token(&token, input)?;
        let signed_exp = match op {
            '*' | '.' => exp,
            '/' => -exp,
            other => {
                return Err(ParseError {
                    input: input.to_string(),
                    reason: format!("unexpected operator {other:?}"),
                });
            }
        };
        dim = dim.mul(token_dim.pow(signed_exp));
        scale *= token_scale.powi(signed_exp as i32);

        // Consume the next operator (if any).
        match chars.peek() {
            None => break,
            Some(&c) if c == '*' || c == '.' || c == '/' => {
                op = c;
                chars.next();
            }
            Some(&c) => {
                return Err(ParseError {
                    input: input.to_string(),
                    reason: format!("unexpected character {c:?} after token {token:?}"),
                });
            }
        }
    }

    Ok(ParsedUnit { dim, scale })
}

/// Compare two unit strings for dimensional + scale equality. Returns
/// `Ok(true)` only when both parse and produce the same `(dim, scale)`
/// tuple. `Ok(false)` means they parse but differ. `Err` means one of the
/// strings doesn't parse — the caller decides whether to treat that as a
/// mismatch.
///
/// Note: a `None` declared unit on either side of a wire is *not* this
/// function's concern; the orchestrator decides what to do with `None`.
/// This function only operates on two parseable strings.
pub fn units_equal(a: &str, b: &str) -> Result<bool, ParseError> {
    let pa = parse_unit(a)?;
    let pb = parse_unit(b)?;
    Ok(pa.dim == pb.dim && pa.scale == pb.scale)
}

/// Sentinel string substituted into the mismatch message when one side of a
/// wire declares no unit. Surfaces verbatim in the error so the operator can
/// see *which* side was undeclared.
pub const UNKNOWN_UNIT_DISPLAY: &str = "<unknown>";

/// Build the mismatch-error message produced by orchestrator pass 1 when a
/// declared input unit does not match the upstream-declared output unit.
///
/// The message contains every field needed to locate the bad wire: the
/// destination calc kind, the destination input field name, the upstream
/// source name, both unit strings (or `<unknown>` when `None`), and a
/// human-readable dimensional reason. When both sides parse as temperature
/// but their scales differ (the K↔°C case), a copy-pasteable `Affine`
/// hint is appended so the operator can fix the wire without re-deriving
/// the offset themselves.
///
/// Arguments:
/// - `dest_calc_kind`: the receiving calc's [`Calc::kind`] (e.g., `"Affine"`)
/// - `dest_input_field`: the receiving calc's input field name (e.g., `"x"`)
/// - `source_name`: the upstream channel's full field name (e.g., `"rtd.K"`)
/// - `dest_unit`: what the receiving calc declared (`get_input_units()`)
/// - `source_unit`: what the upstream produces (peripheral output or upstream
///   calc's `get_output_units()`)
pub fn unit_mismatch_message(
    dest_calc_kind: &str,
    dest_input_field: &str,
    source_name: &str,
    dest_unit: Option<&str>,
    source_unit: Option<&str>,
) -> String {
    let dest_str = dest_unit.unwrap_or(UNKNOWN_UNIT_DISPLAY);
    let source_str = source_unit.unwrap_or(UNKNOWN_UNIT_DISPLAY);

    // Try to parse both sides so the dimensional reason can be specific.
    // Either side may be `<unknown>` (None) or unparseable; both cases fall
    // back to a string-only reason.
    let dest_parsed = dest_unit.and_then(|u| parse_unit(u).ok());
    let source_parsed = source_unit.and_then(|u| parse_unit(u).ok());

    let reason = match (&dest_parsed, &source_parsed) {
        (Some(d), Some(s)) if d.dim == s.dim && d.scale != s.scale => {
            format!(
                "{0} dimension matches but scale differs ({1} vs {2})",
                d.dim.describe(),
                source_str,
                dest_str,
            )
        }
        (Some(d), Some(s)) => format!(
            "different dimensions ({0} vs {1})",
            s.dim.describe(),
            d.dim.describe(),
        ),
        // One or both sides unparseable / unknown — echo the strings.
        _ => format!("expected {dest_str}, got {source_str}"),
    };

    let mut msg = format!(
        "Unit mismatch wiring `{source_name}` (declared {source_str}) into \
         `{dest_calc_kind}` input `{dest_input_field}` (expected {dest_str}): {reason}"
    );

    // K↔°C affine hint: only when both parse, both are temperature, and the
    // scales differ (one is K = 1.0, the other is °C/degC = TEMP_C_SCALE).
    if let (Some(d), Some(s)) = (&dest_parsed, &source_parsed) {
        let is_temp = d.dim.temperature == 1
            && d.dim.mass == 0
            && d.dim.length == 0
            && d.dim.time == 0
            && d.dim.current == 0
            && d.dim.amount == 0
            && d.dim.luminous == 0;
        if is_temp && d.dim == s.dim && d.scale != s.scale {
            // Determine direction by comparing scales against the K base
            // (scale 1.0) vs. °C marker (TEMP_C_SCALE).
            let source_is_kelvin = s.scale == 1.0;
            let dest_is_kelvin = d.scale == 1.0;
            let hint = if source_is_kelvin && !dest_is_kelvin {
                // K → °C: subtract 273.15 to convert.
                "K and °C are dimensionally compatible but require an offset \
                 conversion; insert an Affine(scale=1.0, offset=-273.15) \
                 between source and dest"
            } else if !source_is_kelvin && dest_is_kelvin {
                // °C → K: add 273.15.
                "°C and K are dimensionally compatible but require an offset \
                 conversion; insert an Affine(scale=1.0, offset=273.15) \
                 between source and dest"
            } else {
                // Both °C-ish or both K-ish: shouldn't happen given scale check above.
                ""
            };
            if !hint.is_empty() {
                msg.push_str(" — ");
                msg.push_str(hint);
            }
        }
    }

    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_base() {
        let p = parse_unit("K").unwrap();
        assert_eq!(p.dim.temperature, 1);
        assert_eq!(p.scale, 1.0);
    }

    #[test]
    fn parses_dimensionless() {
        let p = parse_unit("1").unwrap();
        assert_eq!(p.dim, Dimension::default());
        assert_eq!(p.scale, 1.0);
    }

    #[test]
    fn newton_round_trip() {
        // N == kg·m/s^2
        assert!(units_equal("N", "kg.m/s^2").unwrap());
        assert!(units_equal("N", "kg*m/s^2").unwrap());
        let p = parse_unit("kg.m/s^2").unwrap();
        assert_eq!(p.dim.mass, 1);
        assert_eq!(p.dim.length, 1);
        assert_eq!(p.dim.time, -2);
        assert_eq!(p.scale, 1.0);
    }

    #[test]
    fn pascal_round_trip() {
        // Pa == kg/(m·s^2) which we have to write as kg/m/s^2 (no parens in v1)
        assert!(units_equal("Pa", "kg/m/s^2").unwrap());
    }

    #[test]
    fn kpa_is_1000_pa() {
        let kpa = parse_unit("kPa").unwrap();
        let pa = parse_unit("Pa").unwrap();
        assert_eq!(kpa.dim, pa.dim);
        assert_eq!(kpa.scale, 1000.0);
    }

    #[test]
    fn kpa_neq_pa() {
        // Same dimension, different scale.
        assert!(!units_equal("kPa", "Pa").unwrap());
    }

    #[test]
    fn mpa_is_milli_pascal() {
        // `mPa` means millipascal under our prefix allow-list, NOT meter-pascal.
        let mpa = parse_unit("mPa").unwrap();
        let pa = parse_unit("Pa").unwrap();
        assert_eq!(mpa.dim, pa.dim);
        assert_eq!(mpa.scale, 1e-3);
    }

    #[test]
    fn megapascal_is_1e6_pa() {
        let mega = parse_unit("MPa").unwrap();
        assert_eq!(mega.scale, 1e6);
    }

    #[test]
    fn unknown_unit_echoes_input() {
        let err = parse_unit("furlongs").unwrap_err();
        assert_eq!(err.input, "furlongs");
        // The display impl quotes the input verbatim too.
        assert!(err.to_string().contains("furlongs"));
    }

    #[test]
    fn unknown_unit_in_compound_echoes_input() {
        let err = parse_unit("furlongs/fortnight").unwrap_err();
        assert_eq!(err.input, "furlongs/fortnight");
        assert!(err.to_string().contains("furlongs/fortnight"));
    }

    #[test]
    fn mk_is_ambiguous_and_rejected() {
        // `mK` would be milli-Kelvin if K were on the prefix allow-list, but
        // it isn't (we reject prefixes on K to keep it unambiguous with `m`
        // the base meter unit). So `mK` parses as the unknown token `mK`.
        let err = parse_unit("mK").unwrap_err();
        assert_eq!(err.input, "mK");
    }

    #[test]
    fn temperature_units_reject_si_prefixes() {
        // The TEMP_C_SCALE = 1.0e9 marker is only safe because no SI prefix is
        // allowed on K/°C/degC. If any of these started parsing successfully
        // we'd have a scale collision (e.g., a hypothetical kK at scale=1e3
        // would still be temperature dimension and could mask K vs °C handling).
        // Lock the rejection in.
        assert!(parse_unit("mK").is_err());
        assert!(parse_unit("kK").is_err());
        assert!(parse_unit("m°C").is_err());
    }

    #[test]
    fn celsius_and_kelvin_have_different_scales() {
        // K and °C are dimensionally compatible but treated as unequal so
        // the orchestrator can attach the K↔°C affine hint.
        let k = parse_unit("K").unwrap();
        let c = parse_unit("°C").unwrap();
        assert_eq!(k.dim, c.dim);
        assert_ne!(k.scale, c.scale);
        assert!(!units_equal("K", "°C").unwrap());
        assert!(!units_equal("K", "degC").unwrap());
    }

    #[test]
    fn celsius_aliases_match_each_other() {
        assert!(units_equal("°C", "degC").unwrap());
    }

    #[test]
    fn whitespace_is_ignored() {
        assert!(units_equal("kg.m / s^2", "N").unwrap());
        assert!(units_equal("  K  ", "K").unwrap());
    }

    #[test]
    fn empty_string_is_error() {
        let err = parse_unit("").unwrap_err();
        assert_eq!(err.input, "");
    }

    #[test]
    fn negative_exponent() {
        // Hz == s^-1 — both should produce dim time = -1, scale = 1.
        assert!(units_equal("Hz", "s^-1").unwrap());
    }

    #[test]
    fn dot_and_star_are_equivalent() {
        assert!(units_equal("kg.m", "kg*m").unwrap());
    }

    #[test]
    fn double_quotient_is_left_associative() {
        // `kg/m/s^2` == `kg/(m·s^2)` == Pa under left-to-right reading.
        // Each `/` flips the sign of the *next* token's exponent only,
        // matching UCUM's left-to-right reading.
        let p = parse_unit("kg/m/s^2").unwrap();
        assert_eq!(p.dim.mass, 1);
        assert_eq!(p.dim.length, -1);
        assert_eq!(p.dim.time, -2);
    }

    #[test]
    fn rad_is_dimensionless() {
        // Radians are dimensionless in physics; equality with `1` confirms it.
        assert!(units_equal("rad", "1").unwrap());
    }

    #[test]
    fn coulomb_charge_is_a_times_s() {
        assert!(units_equal("C", "A.s").unwrap());
    }

    #[test]
    fn micro_volt_unicode_and_ascii_match() {
        let ascii = parse_unit("uV").unwrap();
        let unicode = parse_unit("µV").unwrap();
        assert_eq!(ascii, unicode);
    }

    #[test]
    fn mismatch_message_includes_required_fields() {
        // Pa upstream → K downstream: different dimensions. The message must
        // name the source, dest calc kind, dest input field, both unit
        // strings, and a dimensional reason.
        let msg = unit_mismatch_message("Affine", "x", "rtd.K", Some("K"), Some("Pa"));
        assert!(msg.contains("Affine"), "missing dest calc kind: {msg}");
        assert!(msg.contains("`x`"), "missing dest input field: {msg}");
        assert!(msg.contains("rtd.K"), "missing source name: {msg}");
        assert!(msg.contains("K"), "missing dest unit: {msg}");
        assert!(msg.contains("Pa"), "missing source unit: {msg}");
        // Dimensional reason — the dimensions differ. Require the dimension
        // words from `Dimension::describe()` (here `temperature` for K and
        // `mass`/`length`/`time` from Pa's decomposition) so a regression
        // that drops `describe()` and falls back to the unit-strings-only
        // path would fail this test. The "different dimensions" wrapper
        // alone is not enough — it doesn't witness that `describe()` ran.
        assert!(
            msg.contains("temperature"),
            "missing dest dimension name: {msg}"
        );
        assert!(
            msg.contains("mass") && msg.contains("length") && msg.contains("time"),
            "missing source dimension names: {msg}"
        );
    }

    #[test]
    fn mismatch_message_unknown_side_uses_sentinel() {
        let msg = unit_mismatch_message("Affine", "x", "rtd.unknown", Some("K"), None);
        assert!(msg.contains(UNKNOWN_UNIT_DISPLAY));
    }

    #[test]
    fn k_to_celsius_message_has_affine_hint() {
        // Source K → dest °C: subtract 273.15.
        let msg = unit_mismatch_message("Affine", "x", "rtd.K", Some("°C"), Some("K"));
        assert!(msg.contains("K"), "missing K: {msg}");
        assert!(msg.contains("°C"), "missing °C: {msg}");
        assert!(msg.contains("offset=-273.15"), "missing affine hint: {msg}");
    }

    #[test]
    fn celsius_to_k_message_has_reverse_affine_hint() {
        // Source °C → dest K: add 273.15.
        let msg = unit_mismatch_message("Affine", "x", "src.celsius", Some("K"), Some("°C"));
        assert!(msg.contains("offset=273.15"), "missing affine hint: {msg}");
        // Make sure we got the positive offset, not the negative one.
        assert!(!msg.contains("offset=-273.15"));
    }

    #[test]
    fn unparseable_dest_unit_falls_back_to_string_reason() {
        // Dest declares `furlongs` (unparseable). The reason should fall
        // back to echoing the strings rather than blowing up.
        let msg = unit_mismatch_message("Affine", "x", "src.K", Some("furlongs"), Some("K"));
        assert!(msg.contains("furlongs"));
        assert!(msg.contains("K"));
    }
}
