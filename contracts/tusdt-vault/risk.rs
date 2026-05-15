use super::*;

impl TusdtVault {
    pub(crate) fn collateral_value(price: Ratio, collateral_balance: Balance) -> Result<Balance> {
        let collateral_value_in_borrow = price
            .checked_mul_value(u128::from(collateral_balance))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(collateral_value_in_borrow).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn max_borrow_allowed(
        &self,
        price: Ratio,
        collateral_balance: Balance,
    ) -> Result<Balance> {
        let collateral_value_in_borrow = Self::collateral_value(price, collateral_balance)?;
        let max = self
            .params
            .collateral_ratio
            .checked_div_value(u128::from(collateral_value_in_borrow))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(max).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn liquidation_limit(
        &self,
        price: Ratio,
        collateral_balance: Balance,
    ) -> Result<Balance> {
        let collateral_value_in_borrow = Self::collateral_value(price, collateral_balance)?;
        let limit = self
            .params
            .liquidation_ratio
            .checked_div_value(u128::from(collateral_value_in_borrow))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(limit).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn is_liquidatable(&self, price: Ratio, vault: &Vault) -> Result<bool> {
        let limit = self.liquidation_limit(price, vault.collateral_balance)?;
        Ok(vault.debt_balance > limit)
    }

    pub(crate) fn liquidation_min_bid(&self, debt_balance: Balance) -> Result<Balance> {
        let liquidation_fee = self
            .params
            .liquidation_fee
            .checked_mul_value(u128::from(debt_balance))
            .ok_or(Error::ArithmeticError)?;
        let min_bid = u128::from(debt_balance)
            .checked_add(liquidation_fee)
            .ok_or(Error::ArithmeticError)?;

        Balance::try_from(min_bid).map_err(|_| Error::ArithmeticError)
    }

    /// Validates a collateral addition (create or top-up) against min, per-vault cap, and global cap.
    /// Returns the projected (vault_balance, total_balance) the caller must commit on success.
    pub(crate) fn ensure_collateral_bounds(
        &self,
        vault_current: Balance,
        addition: Balance,
    ) -> Result<(Balance, Balance)> {
        if addition == 0 {
            return Err(Error::InsufficientCollateral);
        }

        let projected_vault = vault_current
            .checked_add(addition)
            .ok_or(Error::ArithmeticError)?;
        if projected_vault < self.params.min_vault_collateral {
            return Err(Error::InsufficientCollateral);
        }
        if projected_vault > self.params.max_vault_collateral {
            return Err(Error::CollateralCapExceeded);
        }

        let projected_total = self
            .total_collateral_balance
            .checked_add(addition)
            .ok_or(Error::ArithmeticError)?;
        if projected_total > self.params.max_total_collateral {
            return Err(Error::CollateralCapExceeded);
        }

        Ok((projected_vault, projected_total))
    }
}
