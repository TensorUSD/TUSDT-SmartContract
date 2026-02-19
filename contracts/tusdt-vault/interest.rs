use super::*;
use ink::codegen::Env as _;
use tusdt_primitives::{DAYS_PER_YEAR, SECONDS_PER_DAY};

impl TusdtVault {
    pub(crate) fn accrue_interest(&self, vault: &mut Vault) -> Result<()> {
        let now = self.env().block_timestamp();
        if now <= vault.last_interest_accrued_at {
            return Ok(());
        }
        if vault.borrowed_token_balance == 0 || self.params.interest_rate.is_zero() {
            vault.last_interest_accrued_at = now;
            return Ok(());
        }

        // We checked that now > vault.last_interest_accrued_at.
        #[allow(clippy::arithmetic_side_effects)]
        let elapsed = (now - vault.last_interest_accrued_at) as u128;
        let borrowed_days = elapsed
            .checked_div(SECONDS_PER_DAY as u128)
            .ok_or(Error::ArithmeticError)?;
        if borrowed_days == 0 {
            return Ok(());
        }

        let daily_exponent = self
            .params
            .interest_rate
            .checked_div_int(DAYS_PER_YEAR)
            .ok_or(Error::ArithmeticError)?;

        let daily_growth_factor = daily_exponent.exp().ok_or(Error::ArithmeticError)?;
        let compounded_growth_factor = daily_growth_factor
            .checked_pow(borrowed_days)
            .ok_or(Error::ArithmeticError)?;

        vault.borrowed_token_balance = compounded_growth_factor
            .checked_mul_value(vault.borrowed_token_balance)
            .ok_or(Error::ArithmeticError)?;

        let accrued_seconds = borrowed_days
            .checked_mul(SECONDS_PER_DAY as u128)
            .ok_or(Error::ArithmeticError)?
            .checked_add(vault.last_interest_accrued_at as u128)
            .ok_or(Error::ArithmeticError)?;
        if accrued_seconds > u64::MAX as u128 {
            return Err(Error::ArithmeticError);
        }
        vault.last_interest_accrued_at = accrued_seconds as u64;

        Ok(())
    }
}
