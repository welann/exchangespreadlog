use std::{cmp::Ordering, fmt, str::FromStr};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fixed {
    value: i128,
    scale: u32,
}

impl PartialOrd for Fixed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Fixed {
    fn cmp(&self, other: &Self) -> Ordering {
        let scale = self.scale.max(other.scale);
        match (self.align_value(scale), other.align_value(scale)) {
            (Ok(lhs), Ok(rhs)) => lhs.cmp(&rhs),
            _ => self
                .to_f64()
                .partial_cmp(&other.to_f64())
                .unwrap_or(Ordering::Equal)
                .then_with(|| self.scale.cmp(&other.scale))
                .then_with(|| self.value.cmp(&other.value)),
        }
    }
}

impl Fixed {
    pub fn new(value: i128, scale: u32) -> Self {
        Self { value, scale }
    }

    pub fn value(self) -> i128 {
        self.value
    }

    pub fn scale(self) -> u32 {
        self.scale
    }

    pub fn to_f64(self) -> f64 {
        self.value as f64 / 10_f64.powi(self.scale as i32)
    }

    pub fn checked_sub(self, rhs: Self) -> Result<Self> {
        let scale = self.scale.max(rhs.scale);
        let lhs = self.align_value(scale)?;
        let rhs = rhs.align_value(scale)?;
        Ok(Self::new(lhs - rhs, scale).normalized())
    }

    pub fn midpoint(self, rhs: Self) -> Result<Self> {
        let scale = self.scale.max(rhs.scale) + 1;
        let lhs = self.align_value(scale)?;
        let rhs = rhs.align_value(scale)?;
        Ok(Self::new((lhs + rhs) / 2, scale).normalized())
    }

    fn align_value(self, target_scale: u32) -> Result<i128> {
        if target_scale < self.scale {
            bail!(
                "target scale {target_scale} is lower than fixed scale {}",
                self.scale
            );
        }
        let diff = target_scale - self.scale;
        let factor = 10_i128
            .checked_pow(diff)
            .ok_or_else(|| anyhow!("scale {target_scale} is too large"))?;
        self.value
            .checked_mul(factor)
            .ok_or_else(|| anyhow!("fixed-point value overflow"))
    }

    fn normalized(mut self) -> Self {
        while self.scale > 0 && self.value % 10 == 0 {
            self.value /= 10;
            self.scale -= 1;
        }
        self
    }
}

impl FromStr for Fixed {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            bail!("empty decimal string");
        }

        let (negative, unsigned) = match input.as_bytes()[0] {
            b'-' => (true, &input[1..]),
            b'+' => (false, &input[1..]),
            _ => (false, input),
        };

        if unsigned.is_empty() {
            bail!("decimal string has no digits");
        }

        let parts: Vec<&str> = unsigned.split('.').collect();
        if parts.len() > 2 {
            bail!("invalid decimal string: {input}");
        }

        let int_part = parts[0];
        let frac_part = parts.get(1).copied().unwrap_or("");
        if int_part.is_empty() && frac_part.is_empty() {
            bail!("decimal string has no digits");
        }
        if !int_part.chars().all(|c| c.is_ascii_digit())
            || !frac_part.chars().all(|c| c.is_ascii_digit())
        {
            bail!("invalid decimal digit in {input}");
        }

        let digits = format!(
            "{}{}",
            if int_part.is_empty() { "0" } else { int_part },
            frac_part
        );
        let mut value = digits.parse::<i128>()?;
        if negative {
            value = -value;
        }

        Ok(Self::new(value, frac_part.len() as u32).normalized())
    }
}

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let negative = self.value < 0;
        let digits = self.value.abs().to_string();

        if self.scale == 0 {
            return write!(f, "{}{digits}", if negative { "-" } else { "" });
        }

        let scale = self.scale as usize;
        if digits.len() <= scale {
            let padding = "0".repeat(scale - digits.len());
            write!(f, "{}0.{padding}{digits}", if negative { "-" } else { "" })
        } else {
            let split = digits.len() - scale;
            write!(
                f,
                "{}{}.{}",
                if negative { "-" } else { "" },
                &digits[..split],
                &digits[split..]
            )
        }
    }
}

impl Serialize for Fixed {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Fixed {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::Fixed;

    #[test]
    fn parses_and_normalizes_decimal_strings() {
        let fixed: Fixed = "00123.4500".parse().unwrap();
        assert_eq!(fixed.value(), 12345);
        assert_eq!(fixed.scale(), 2);
        assert_eq!(fixed.to_string(), "123.45");
    }

    #[test]
    fn calculates_spread_and_midpoint_without_float_error() {
        let bid: Fixed = "100.10".parse().unwrap();
        let ask: Fixed = "100.12".parse().unwrap();

        assert_eq!(ask.checked_sub(bid).unwrap().to_string(), "0.02");
        assert_eq!(bid.midpoint(ask).unwrap().to_string(), "100.11");
    }

    #[test]
    fn orders_values_by_numeric_value_across_scales() {
        let lower: Fixed = "100.5".parse().unwrap();
        let higher: Fixed = "101".parse().unwrap();

        assert!(lower < higher);
    }

    #[test]
    fn converts_to_f64_for_display_charts() {
        let fixed: Fixed = "-12.345".parse().unwrap();
        assert!((fixed.to_f64() + 12.345).abs() < f64::EPSILON);
    }
}
