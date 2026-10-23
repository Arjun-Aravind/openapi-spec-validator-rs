//! A minimal signed big integer, enough to mirror Python `int` arithmetic in the constructor.

use std::cmp::Ordering;

/// Sign-magnitude integer; `mag` is little-endian base 2^32 without trailing zero limbs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub(super) struct BigInt {
    neg: bool,
    mag: Vec<u32>,
}

impl BigInt {
    pub fn from_i128(v: i128) -> Self {
        let mut abs = v.unsigned_abs();
        let mut mag = Vec::new();
        while abs != 0 {
            mag.push(abs as u32);
            abs >>= 32;
        }
        Self { neg: v < 0, mag }
    }

    /// Parses ASCII digits (already validated for `radix`).
    pub fn from_digits(digits: &[u8], radix: u32) -> Self {
        let mut out = Self::default();
        for &d in digits {
            let v = char::from(d).to_digit(radix).unwrap_or(0);
            out.mul_add_small(radix, v);
        }
        out
    }

    /// The exact value of an integral, finite float.
    pub fn from_integral_f64(f: f64) -> Self {
        let bits = f.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & ((1 << 52) - 1);
        let (mantissa, shift) = if exp == 0 {
            (frac, -1074)
        } else {
            (frac | (1 << 52), exp - 1075)
        };
        let mut out = Self::from_i128(i128::from(mantissa));
        if shift >= 0 {
            for _ in 0..shift {
                out.mul_add_small(2, 0);
            }
        } else {
            // Integral floats with a negative binary exponent fit in a u64 mantissa shift.
            out = Self::from_i128(i128::from(mantissa >> (-shift).min(63)));
        }
        out.neg = f < 0.0 && !out.mag.is_empty();
        out
    }

    fn mul_add_small(&mut self, m: u32, a: u32) {
        let mut carry = u64::from(a);
        for limb in &mut self.mag {
            let v = u64::from(*limb) * u64::from(m) + carry;
            *limb = v as u32;
            carry = v >> 32;
        }
        if carry != 0 {
            self.mag.push(carry as u32);
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
    }

    pub fn negated(mut self) -> Self {
        if !self.mag.is_empty() {
            self.neg = !self.neg;
        }
        self
    }

    pub fn mul_small(&self, m: u32) -> Self {
        let mut out = self.clone();
        out.mul_add_small(m, 0);
        out.neg = self.neg && !out.mag.is_empty();
        out
    }

    fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
        a.len()
            .cmp(&b.len())
            .then_with(|| a.iter().rev().cmp(b.iter().rev()))
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
        let mut out = Vec::with_capacity(long.len() + 1);
        let mut carry = 0u64;
        for (i, &x) in long.iter().enumerate() {
            let v = u64::from(x) + u64::from(short.get(i).copied().unwrap_or(0)) + carry;
            out.push(v as u32);
            carry = v >> 32;
        }
        if carry != 0 {
            out.push(carry as u32);
        }
        out
    }

    /// `a - b` for `|a| >= |b|`.
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len());
        let mut borrow = 0i64;
        for (i, &x) in a.iter().enumerate() {
            let mut v = i64::from(x) - i64::from(b.get(i).copied().unwrap_or(0)) - borrow;
            borrow = 0;
            if v < 0 {
                v += 1 << 32;
                borrow = 1;
            }
            out.push(v as u32);
        }
        out
    }

    pub fn add(&self, other: &Self) -> Self {
        let mut out = if self.neg == other.neg {
            Self {
                neg: self.neg,
                mag: Self::add_mag(&self.mag, &other.mag),
            }
        } else {
            match Self::cmp_mag(&self.mag, &other.mag) {
                Ordering::Less => Self {
                    neg: other.neg,
                    mag: Self::sub_mag(&other.mag, &self.mag),
                },
                _ => Self {
                    neg: self.neg,
                    mag: Self::sub_mag(&self.mag, &other.mag),
                },
            }
        };
        out.trim();
        out
    }

    pub fn to_i64(&self) -> Option<i64> {
        let abs = self.to_u128_abs()?;
        if self.neg {
            0i64.checked_sub_unsigned(u64::try_from(abs).ok()?)
                .or_else(|| (abs == 1u128 << 63).then_some(i64::MIN))
        } else {
            i64::try_from(abs).ok()
        }
    }

    pub fn to_u64(&self) -> Option<u64> {
        if self.neg {
            return None;
        }
        u64::try_from(self.to_u128_abs()?).ok()
    }

    pub fn to_i128(&self) -> Option<i128> {
        let abs = i128::try_from(self.to_u128_abs()?).ok()?;
        Some(if self.neg { -abs } else { abs })
    }

    fn to_u128_abs(&self) -> Option<u128> {
        if self.mag.len() > 4 {
            return None;
        }
        Some(
            self.mag
                .iter()
                .rev()
                .fold(0u128, |acc, &limb| (acc << 32) | u128::from(limb)),
        )
    }

    /// Decimal digits of `|self|`.
    pub fn abs_decimal(&self) -> String {
        if self.mag.is_empty() {
            return "0".to_owned();
        }
        let mut mag = self.mag.clone();
        let mut chunks = Vec::new();
        while !mag.is_empty() {
            let mut rem = 0u64;
            for limb in mag.iter_mut().rev() {
                let cur = (rem << 32) | u64::from(*limb);
                *limb = (cur / 1_000_000_000) as u32;
                rem = cur % 1_000_000_000;
            }
            while mag.last() == Some(&0) {
                mag.pop();
            }
            chunks.push(rem as u32);
        }
        let mut out = String::with_capacity(chunks.len() * 9);
        let mut iter = chunks.iter().rev();
        if let Some(first) = iter.next() {
            out.push_str(&first.to_string());
        }
        for chunk in iter {
            out.push_str(&format!("{chunk:09}"));
        }
        out
    }

    pub fn to_decimal(&self) -> String {
        let digits = self.abs_decimal();
        if self.neg {
            format!("-{digits}")
        } else {
            digits
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_round_trip() {
        let n = BigInt::from_digits(b"123456789012345678901234567890", 10);
        assert_eq!(n.to_decimal(), "123456789012345678901234567890");
        let h = BigInt::from_digits(b"ffffffffffffffffffff", 16);
        assert_eq!(h.to_decimal(), "1208925819614629174706175");
        let sum = h.add(&n.clone().negated());
        assert_eq!(sum.to_decimal(), "-123455580086526064272059861715");
        assert_eq!(
            BigInt::from_i128(i128::from(i64::MIN)).to_i64(),
            Some(i64::MIN)
        );
        assert_eq!(BigInt::from_i128(-5).to_i64(), Some(-5));
        assert_eq!(
            BigInt::from_integral_f64(1e20).to_decimal(),
            "100000000000000000000"
        );
        assert_eq!(BigInt::from_integral_f64(-3.0).to_decimal(), "-3");
    }
}
