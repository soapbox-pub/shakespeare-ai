//! Amounts of US dollars, held as whole nanodollars.
//!
//! Credits are spent a few millionths of a dollar at a time, so cents are far
//! too coarse, and floats drift. An `i64` of nanodollars is exact, sums in
//! SQLite without ceremony, and still holds nine billion dollars. Arithmetic
//! on rates (prices, fees, exchange rates) is done in `Decimal` and rounded to
//! the nanodollar once, at the end.

use std::fmt;
use std::str::FromStr;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const NANOS_PER_DOLLAR: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Money(pub i64);

impl Money {
    pub const ZERO: Money = Money(0);

    pub fn nanos(self) -> i64 {
        self.0
    }

    /// `None` if it's more nanodollars than an `i64` holds. Checked, as
    /// amounts come from requests: `Decimal`'s `*` panics on overflow.
    pub fn from_dollars(dollars: Decimal) -> Option<Money> {
        let nanos = dollars.checked_mul(Decimal::from(NANOS_PER_DOLLAR))?.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
        nanos.to_i64().map(Money)
    }

    pub fn to_decimal(self) -> Decimal {
        Decimal::from(self.0) / Decimal::from(NANOS_PER_DOLLAR)
    }

    /// As a JSON number, which is how the API has always spelled amounts.
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / NANOS_PER_DOLLAR as f64
    }

    /// Whole cents, rounded up: what a card is charged for this amount.
    pub fn to_cents_ceil(self) -> i64 {
        (self.0 + 9_999_999).div_euclid(10_000_000)
    }

    /// Rounded to the cent, half away from zero, as fees always have been.
    pub fn round_to_cents(self) -> Money {
        Money::from_dollars(self.to_decimal().round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)).unwrap_or(self)
    }

    pub fn checked_add(self, other: Money) -> Option<Money> {
        self.0.checked_add(other.0).map(Money)
    }

    pub fn checked_mul(self, n: i64) -> Option<Money> {
        self.0.checked_mul(n).map(Money)
    }

    pub fn is_positive(self) -> bool {
        self.0 > 0
    }
}

impl std::ops::Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(-self.0)
    }
}

impl fmt::Display for Money {
    /// Dollars, to the cent: for people, not for the API.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.to_decimal().round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero))
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.to_f64())
    }
}

/// A decimal written as a number or a string in a config file or a request.
///
/// A float is converted through its shortest round-tripping spelling, so
/// `0.011` becomes exactly 0.011 rather than the binary fraction nearest it.
pub fn decimal_from_json_number(value: &serde_json::Value) -> Option<Decimal> {
    match value {
        serde_json::Value::Number(n) => Decimal::from_str(&n.to_string()).ok().or_else(|| n.as_f64().and_then(|f| Decimal::from_str(&f.to_string()).ok())),
        serde_json::Value::String(s) => Decimal::from_str(s.trim()).ok(),
        _ => None,
    }
}

/// Deserialize a `Decimal` from a YAML or JSON number or string.
pub fn deserialize_decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Int(i64),
        Float(f64),
        Str(String),
    }
    let parsed = match Raw::deserialize(deserializer)? {
        Raw::Int(i) => Some(Decimal::from(i)),
        Raw::Float(f) => Decimal::from_str(&f.to_string()).ok(),
        Raw::Str(s) => Decimal::from_str(s.trim()).ok(),
    };
    parsed.ok_or_else(|| serde::de::Error::custom("expected a decimal number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts() {
        let ten_fifty = Money::from_dollars(Decimal::from_str("10.50").unwrap()).unwrap();
        assert_eq!(ten_fifty.0, 10_500_000_000);
        assert_eq!(ten_fifty.to_f64(), 10.5);
        assert_eq!(ten_fifty.to_string(), "10.50");
        assert_eq!(Money(1).to_cents_ceil(), 1);
        assert_eq!(Money(10_000_000).to_cents_ceil(), 1);
        assert_eq!(Money(10_000_001).to_cents_ceil(), 2);
        assert_eq!(Money::from_dollars(Decimal::from_str("0.0000000005").unwrap()).unwrap().0, 1);
        assert_eq!(Money::from_dollars(Decimal::from_str("1.005").unwrap()).unwrap().round_to_cents().0, 1_010_000_000);
        assert_eq!(Money::from_dollars(Decimal::MAX), None);
        assert_eq!(Money::from_dollars(Decimal::from_str("10000000000").unwrap()), None);
    }
}
