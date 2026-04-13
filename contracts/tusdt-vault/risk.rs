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
        Ok(vault.borrowed_token_balance > limit)
    }

    pub(crate) fn collateral_needed_for_debt(
        price: Ratio,
        debt_balance: Balance,
    ) -> Result<Balance> {
        let collateral_balance = price
            .checked_div_value(u128::from(debt_balance))
            .ok_or(Error::ArithmeticError)?;
        Balance::try_from(collateral_balance).map_err(|_| Error::ArithmeticError)
    }

    pub(crate) fn collateral_to_auction(
        &self,
        price: Ratio,
        debt_balance: Balance,
        collateral_balance: Balance,
    ) -> Result<Balance> {
        let collateral_cap = u128::from(collateral_balance);
        let collateral_debt = match Self::collateral_needed_for_debt(price, debt_balance) {
            Ok(collateral_debt) => collateral_debt,
            Err(Error::ArithmeticError) => return Ok(collateral_balance),
            Err(error) => return Err(error),
        };
        let collateral_debt = u128::from(collateral_debt);

        if collateral_debt >= collateral_cap {
            return Ok(collateral_balance);
        }

        let liquidation_fee = self
            .params
            .liquidation_fee
            .checked_mul_value(collateral_debt)
            .ok_or(Error::ArithmeticError)?;
        let collateral_to_auction = collateral_debt
            .checked_add(liquidation_fee)
            .ok_or(Error::ArithmeticError)?
            .min(collateral_cap);

        Balance::try_from(collateral_to_auction).map_err(|_| Error::ArithmeticError)
    }
}
