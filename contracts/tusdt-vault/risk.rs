use super::*;

impl TusdtVault {
    pub(crate) fn max_borrow_allowed(&self, collateral_balance: Balance) -> Result<Balance> {
        let collateral_value_in_borrow = self
            .collateral_token_price
            .checked_mul(collateral_balance)
            .ok_or(Error::ArithmeticError)?;
        let max = self
            .params
            .collateral_ratio
            .checked_div_value(u128::from(collateral_value_in_borrow))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(max).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn liquidation_limit(&self, collateral_balance: Balance) -> Result<Balance> {
        let collateral_value_in_borrow = self
            .collateral_token_price
            .checked_mul(collateral_balance)
            .ok_or(Error::ArithmeticError)?;
        let limit = self
            .params
            .liquidation_ratio
            .checked_div_value(u128::from(collateral_value_in_borrow))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(limit).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn is_liquidatable(&self, vault: &Vault) -> Result<bool> {
        let limit = self.liquidation_limit(vault.collateral_balance)?;
        Ok(vault.borrowed_token_balance > limit)
    }
}
