//! Small, dependency-free unit converter for everyday values.
//!
//! Length, mass, temperature, data size, time and speed are supported.  Data
//! units distinguish decimal (KB = 1000 B) from binary (KiB = 1024 B) spellings
//! so a value is never silently rounded to the wrong base.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::error::{FormfluxError, Result};

/// One unit and its factor to the category's base unit.
#[derive(Debug, Clone, Copy)]
struct Unit {
    category: Category,
    factor: f64,
    offset: f64,
}

/// Measurement family used to reject incompatible conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// Length.
    Length,
    /// Mass.
    Mass,
    /// Temperature.
    Temperature,
    /// Digital information.
    Data,
    /// Duration.
    Time,
    /// Speed.
    Speed,
}

impl Category {
    /// Human label.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Length => "length",
            Category::Mass => "mass",
            Category::Temperature => "temperature",
            Category::Data => "data",
            Category::Time => "time",
            Category::Speed => "speed",
        }
    }
}

fn table() -> &'static HashMap<&'static str, Unit> {
    static TABLE: OnceLock<HashMap<&'static str, Unit>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut units = HashMap::new();
        let mut add = |names: &[&'static str], category, factor, offset| {
            for name in names {
                units.insert(
                    *name,
                    Unit {
                        category,
                        factor,
                        offset,
                    },
                );
            }
        };

        // Base: metre.
        add(
            &["mm", "millimetre", "millimeter"],
            Category::Length,
            0.001,
            0.0,
        );
        add(
            &["cm", "centimetre", "centimeter"],
            Category::Length,
            0.01,
            0.0,
        );
        add(&["m", "metre", "meter"], Category::Length, 1.0, 0.0);
        add(
            &["km", "kilometre", "kilometer"],
            Category::Length,
            1000.0,
            0.0,
        );
        add(&["in", "inch", "inches"], Category::Length, 0.0254, 0.0);
        add(&["ft", "foot", "feet"], Category::Length, 0.3048, 0.0);
        add(&["yd", "yard"], Category::Length, 0.9144, 0.0);
        add(&["mi", "mile"], Category::Length, 1609.344, 0.0);

        // Base: gram.
        add(&["mg", "milligram"], Category::Mass, 0.001, 0.0);
        add(&["g", "gram"], Category::Mass, 1.0, 0.0);
        add(&["kg", "kilogram"], Category::Mass, 1000.0, 0.0);
        add(&["t", "tonne", "ton"], Category::Mass, 1_000_000.0, 0.0);
        add(&["oz", "ounce"], Category::Mass, 28.349_523_125, 0.0);
        add(&["lb", "pound"], Category::Mass, 453.592_37, 0.0);

        // Base: degree Celsius.
        add(&["c", "°c", "celsius"], Category::Temperature, 1.0, 0.0);
        add(
            &["f", "°f", "fahrenheit"],
            Category::Temperature,
            5.0 / 9.0,
            -32.0,
        );
        add(&["k", "kelvin"], Category::Temperature, 1.0, -273.15);

        // Base: byte. Decimal and binary spellings are distinct on purpose.
        add(&["b", "byte", "bytes"], Category::Data, 1.0, 0.0);
        add(&["kb"], Category::Data, 1_000.0, 0.0);
        add(&["mb"], Category::Data, 1_000_000.0, 0.0);
        add(&["gb"], Category::Data, 1_000_000_000.0, 0.0);
        add(&["tb"], Category::Data, 1_000_000_000_000.0, 0.0);
        add(&["kib"], Category::Data, 1024.0, 0.0);
        add(&["mib"], Category::Data, 1024.0 * 1024.0, 0.0);
        add(&["gib"], Category::Data, 1024.0 * 1024.0 * 1024.0, 0.0);
        add(
            &["tib"],
            Category::Data,
            1024.0 * 1024.0 * 1024.0 * 1024.0,
            0.0,
        );

        // Base: second.
        add(&["ms", "millisecond"], Category::Time, 0.001, 0.0);
        add(&["s", "sec", "second"], Category::Time, 1.0, 0.0);
        add(&["min", "minute"], Category::Time, 60.0, 0.0);
        add(&["h", "hr", "hour"], Category::Time, 3600.0, 0.0);
        add(&["d", "day"], Category::Time, 86_400.0, 0.0);

        // Base: metre per second.
        add(&["m/s", "mps"], Category::Speed, 1.0, 0.0);
        add(&["km/h", "kph"], Category::Speed, 1.0 / 3.6, 0.0);
        add(&["mph"], Category::Speed, 0.447_04, 0.0);
        add(&["kn", "knot"], Category::Speed, 0.514_444_444, 0.0);

        units
    })
}

/// Normalise spacing and case inside a unit name.
fn normalise(unit: &str) -> String {
    unit.trim().to_ascii_lowercase().replace(' ', "")
}

fn lookup(unit: &str) -> Result<Unit> {
    let key = normalise(unit);
    table()
        .get(key.as_str())
        .copied()
        .ok_or_else(|| FormfluxError::unsupported(format!("unknown unit '{unit}'")))
}

/// Convert `value` from `from` to `to`.
///
/// Temperature uses an affine transform; every other category is a simple
/// ratio.
pub fn convert(value: f64, from: &str, to: &str) -> Result<f64> {
    if !value.is_finite() {
        return Err(FormfluxError::invalid("value must be finite"));
    }
    let source = lookup(from)?;
    let target = lookup(to)?;
    if source.category != target.category {
        return Err(FormfluxError::invalid(format!(
            "cannot convert {} to {}",
            source.category.as_str(),
            target.category.as_str()
        )));
    }
    let base = (value + source.offset) * source.factor;
    Ok(base / target.factor - target.offset)
}

/// Convert and format with up to six fractional digits, trimming zeroes.
pub fn convert_and_format(value: f64, from: &str, to: &str) -> Result<String> {
    let value = convert(value, from, to)?;
    if !value.is_finite() {
        return Err(FormfluxError::invalid("conversion is not finite"));
    }
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    Ok(if text == "-0" { "0".to_string() } else { text })
}

/// Units known for a category, sorted alphabetically.
pub fn units_for(category: Category) -> Vec<&'static str> {
    let mut names = table()
        .iter()
        .filter(|(_, unit)| unit.category == category)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_mass_convert() {
        assert!((convert(1.0, "km", "m").unwrap() - 1000.0).abs() < 1e-9);
        assert!((convert(1.0, "in", "cm").unwrap() - 2.54).abs() < 1e-9);
        assert!((convert(1.0, "kg", "lb").unwrap() - 2.204_622_62).abs() < 1e-6);
    }

    #[test]
    fn temperature_is_affine() {
        assert!((convert(0.0, "c", "f").unwrap() - 32.0).abs() < 1e-9);
        assert!((convert(100.0, "c", "f").unwrap() - 212.0).abs() < 1e-9);
        assert!((convert(32.0, "f", "c").unwrap()).abs() < 1e-9);
        assert!((convert(0.0, "c", "k").unwrap() - 273.15).abs() < 1e-9);
    }

    #[test]
    fn decimal_and_binary_data_units_differ() {
        assert!((convert(1.0, "kb", "b").unwrap() - 1000.0).abs() < 1e-9);
        assert!((convert(1.0, "kib", "b").unwrap() - 1024.0).abs() < 1e-9);
        assert_eq!(convert_and_format(1.5, "gib", "mib").unwrap(), "1536");
    }

    #[test]
    fn incompatible_units_are_rejected() {
        assert!(convert(1.0, "kg", "km").is_err());
        assert!(convert(1.0, "parsec", "m").is_err());
    }

    #[test]
    fn formatted_values_are_tidy() {
        assert_eq!(convert_and_format(1.0, "m", "cm").unwrap(), "100");
        assert_eq!(convert_and_format(-0.0, "m", "cm").unwrap(), "0");
    }
}
