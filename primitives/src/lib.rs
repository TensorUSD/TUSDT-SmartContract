#![cfg_attr(not(feature = "std"), no_std)]

use sp_arithmetic::fixed_point::FixedPointNumber;
use sp_arithmetic::traits::{CheckedAdd, CheckedDiv, CheckedMul, One, Zero};
use sp_arithmetic::FixedU128;

pub const SECONDS_PER_DAY: u64 = 86_400;
pub const DAYS_PER_YEAR: u128 = 365;
const FIXED_SCALE: u128 = <FixedU128 as FixedPointNumber>::DIV;

#[ink::scale_derive(Encode, Decode, TypeInfo)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "std", derive(ink::storage::traits::StorageLayout))]
pub struct Ratio(u128);

impl Ratio {
    pub const fn from_inner(inner: u128) -> Self {
        Self(inner)
    }

    pub fn from_fixed(value: FixedU128) -> Self {
        Self(value.into_inner())
    }

    pub fn from_percentage(percent: u32) -> Self {
        Self(from_percentage(percent).into_inner())
    }

    pub fn to_percentage(self) -> Option<u32> {
        let percent = self.as_fixed().checked_mul_int(100_u128)?;
        u32::try_from(percent).ok()
    }

    pub const fn into_inner(self) -> u128 {
        self.0
    }

    pub const fn one() -> Self {
        Self(FIXED_SCALE)
    }

    pub fn as_fixed(self) -> FixedU128 {
        FixedU128::from_inner(self.0)
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn checked_mul_value(self, value: u128) -> Option<u128> {
        self.as_fixed().checked_mul_int(value)
    }

    pub fn checked_div_value(self, value: u128) -> Option<u128> {
        let value_fixed = FixedU128::checked_from_integer(value)?;
        value_fixed
            .checked_div(&self.as_fixed())?
            .checked_mul_int(1_u128)
    }

    pub fn checked_div_int(self, rhs: u128) -> Option<Self> {
        let rhs_fixed = FixedU128::checked_from_integer(rhs)?;
        self.as_fixed()
            .checked_div(&rhs_fixed)
            .map(Self::from_fixed)
    }

    pub fn exp(self) -> Option<Self> {
        exp_fixed(self.as_fixed()).map(Self::from_fixed)
    }

    pub fn checked_pow(self, exponent: u128) -> Option<Self> {
        pow_fixed(self.as_fixed(), exponent).map(Self::from_fixed)
    }
}

pub fn from_percentage(percent: u32) -> FixedU128 {
    FixedU128::saturating_from_rational(percent as u128, 100_u128)
}

// e ^ exponent with Taylor series.
pub fn exp_fixed(exponent: FixedU128) -> Option<FixedU128> {
    let mut sum = FixedU128::one();
    let mut term = FixedU128::one();

    for n in 1..=32_u128 {
        let next_term = term.checked_mul(&exponent)?;
        let n_fixed = FixedU128::checked_from_integer(n)?;
        term = next_term.checked_div(&n_fixed)?;
        if term.is_zero() {
            break;
        }
        sum = sum.checked_add(&term)?;
    }

    Some(sum)
}

// base ^ exponent using square and multiply.
pub fn pow_fixed(mut base: FixedU128, mut exponent: u128) -> Option<FixedU128> {
    let mut result = FixedU128::one();

    while exponent > 0 {
        if exponent % 2 == 1 {
            result = result.checked_mul(&base)?;
        }
        exponent /= 2;
        if exponent > 0 {
            base = base.checked_mul(&base)?;
        }
    }

    Some(result)
}
