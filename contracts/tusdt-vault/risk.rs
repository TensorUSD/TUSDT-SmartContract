use super::*;

impl TusdtVault {
    pub(crate) fn collateral_value(price: Ratio, collateral_balance: Balance) -> Result<Balance> {
        let collateral_value_in_borrow = price
            .checked_mul_value(u128::from(collateral_balance))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(collateral_value_in_borrow).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn max_borrow_allowed(&self, price: Ratio, collateral_balance: Balance) -> Result<Balance> {
        let collateral_value_in_borrow = Self::collateral_value(price, collateral_balance)?;
        let max = self
            .params
            .collateral_ratio
            .checked_div_value(u128::from(collateral_value_in_borrow))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(max).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn liquidation_limit(&self, price: Ratio, collateral_balance: Balance) -> Result<Balance> {
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
        Ok(vault.borrowed_token_balance > limit)
    }

    pub(crate) fn collateral_needed_for_debt(price: Ratio, debt_balance: Balance) -> Result<Balance> {
        let collateral_balance = price
            .checked_div_value(u128::from(debt_balance))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(collateral_balance).map_err(|_| Error::ArithmeticError)
    }
}
