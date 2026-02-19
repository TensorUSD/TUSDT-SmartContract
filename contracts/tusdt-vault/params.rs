use super::*;

const DEFAULT_COLLATERAL_RATIO_PERCENT: u32 = 150;
const DEFAULT_LIQUIDATION_RATIO_PERCENT: u32 = 120;
const DEFAULT_INTEREST_RATE_PERCENT: u32 = 5;
const DEFAULT_LIQUIDATION_FEE_PERCENT: u32 = 1;

impl TusdtVault {
    pub(crate) fn default_contract_params() -> VaultContractParams {
        let params = VaultContractParams {
            collateral_ratio: Ratio::from_percentage(DEFAULT_COLLATERAL_RATIO_PERCENT),
            liquidation_ratio: Ratio::from_percentage(DEFAULT_LIQUIDATION_RATIO_PERCENT),
            interest_rate: Ratio::from_percentage(DEFAULT_INTEREST_RATE_PERCENT),
            liquidation_fee: Ratio::from_percentage(DEFAULT_LIQUIDATION_FEE_PERCENT),
        };
        Self::validate_contract_params(&params)
            .expect("default vault contract params should be valid");
        params
    }

    pub(crate) fn contract_params_from_percentages(
        params: VaultContractParamsPercentage,
    ) -> Result<VaultContractParams> {
        let ratio_params = VaultContractParams {
            collateral_ratio: Ratio::from_percentage(params.collateral_ratio),
            liquidation_ratio: Ratio::from_percentage(params.liquidation_ratio),
            interest_rate: Ratio::from_percentage(params.interest_rate),
            liquidation_fee: Ratio::from_percentage(params.liquidation_fee),
        };
        Self::validate_contract_params(&ratio_params)?;
        Ok(ratio_params)
    }

    pub(crate) fn contract_params_to_percentages(
        params: VaultContractParams,
    ) -> VaultContractParamsPercentage {
        VaultContractParamsPercentage {
            collateral_ratio: params
                .collateral_ratio
                .to_percentage()
                .expect("stored collateral ratio should fit in u32 percentage"),
            liquidation_ratio: params
                .liquidation_ratio
                .to_percentage()
                .expect("stored liquidation ratio should fit in u32 percentage"),
            interest_rate: params
                .interest_rate
                .to_percentage()
                .expect("stored interest rate should fit in u32 percentage"),
            liquidation_fee: params
                .liquidation_fee
                .to_percentage()
                .expect("stored liquidation fee should fit in u32 percentage"),
        }
    }

    pub(crate) fn validate_contract_params(params: &VaultContractParams) -> Result<()> {
        let one = Ratio::one();
        // Collateral ratio should be greater than or equal to 100%.
        if params.collateral_ratio < one {
            return Err(Error::InvalidRatio);
        }
        // Liquidation ratio should be greater than or equal to 100%.
        if params.liquidation_ratio < one {
            return Err(Error::InvalidRatio);
        }
        // Interest rate should be less than or equal to 100%.
        if params.interest_rate > one {
            return Err(Error::InvalidRatio);
        }
        // Liquidation fee should be less than or equal to 100%.
        if params.liquidation_fee > one {
            return Err(Error::InvalidRatio);
        }
        Ok(())
    }
}
