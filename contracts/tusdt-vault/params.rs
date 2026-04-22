use super::*;

const DEFAULT_COLLATERAL_RATIO_BASIS_POINTS: u32 = 15_000;
const DEFAULT_LIQUIDATION_RATIO_BASIS_POINTS: u32 = 12_000;
const DEFAULT_INTEREST_RATE_BASIS_POINTS: u32 = 500;
const DEFAULT_LIQUIDATION_FEE_BASIS_POINTS: u32 = 100;
const DEFAULT_BORROW_CAP: Balance = 100_000_000_000_000_000; // 100 Million
const DEFAULT_TRANSACTION_FEE_BASIS_POINTS: u32 = 3;
const DEFAULT_AUCTION_DURATION_MS: u64 = 3_600_000;
const DEFAULT_MAX_ORACLE_AGE_MS: u64 = 3_600_000;
const MAX_AUCTION_DURATION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

impl TusdtVault {
    pub(crate) fn default_contract_params() -> VaultContractParams {
        let params = VaultContractParams {
            collateral_ratio: Ratio::from_basis_points(DEFAULT_COLLATERAL_RATIO_BASIS_POINTS),
            liquidation_ratio: Ratio::from_basis_points(DEFAULT_LIQUIDATION_RATIO_BASIS_POINTS),
            interest_rate: Ratio::from_basis_points(DEFAULT_INTEREST_RATE_BASIS_POINTS),
            liquidation_fee: Ratio::from_basis_points(DEFAULT_LIQUIDATION_FEE_BASIS_POINTS),
            borrow_cap: DEFAULT_BORROW_CAP,
            transaction_fee: Ratio::from_basis_points(DEFAULT_TRANSACTION_FEE_BASIS_POINTS),
            auction_duration_ms: DEFAULT_AUCTION_DURATION_MS,
            max_oracle_age_ms: DEFAULT_MAX_ORACLE_AGE_MS,
        };
        Self::validate_contract_params(&params)
            .expect("default vault contract params should be valid");
        params
    }

    pub(crate) fn contract_params_from_config(
        params: VaultContractParamsConfig,
    ) -> Result<VaultContractParams> {
        let config = VaultContractParams {
            collateral_ratio: Ratio::from_basis_points(params.collateral_ratio),
            liquidation_ratio: Ratio::from_basis_points(params.liquidation_ratio),
            interest_rate: Ratio::from_basis_points(params.interest_rate),
            liquidation_fee: Ratio::from_basis_points(params.liquidation_fee),
            borrow_cap: params.borrow_cap,
            transaction_fee: Ratio::from_basis_points(params.transaction_fee),
            auction_duration_ms: params.auction_duration_ms,
            max_oracle_age_ms: params.max_oracle_age_ms,
        };
        Self::validate_contract_params(&config)?;
        Ok(config)
    }

    pub(crate) fn contract_params_to_config(
        params: VaultContractParams,
    ) -> VaultContractParamsConfig {
        VaultContractParamsConfig {
            collateral_ratio: params
                .collateral_ratio
                .to_basis_points()
                .expect("stored collateral ratio should fit in u32 basis points"),
            liquidation_ratio: params
                .liquidation_ratio
                .to_basis_points()
                .expect("stored liquidation ratio should fit in u32 basis points"),
            interest_rate: params
                .interest_rate
                .to_basis_points()
                .expect("stored interest rate should fit in u32 basis points"),
            liquidation_fee: params
                .liquidation_fee
                .to_basis_points()
                .expect("stored liquidation fee should fit in u32 basis points"),
            borrow_cap: params.borrow_cap,
            transaction_fee: params
                .transaction_fee
                .to_basis_points()
                .expect("stored transaction fee should fit in u32 basis points"),
            auction_duration_ms: params.auction_duration_ms,
            max_oracle_age_ms: params.max_oracle_age_ms,
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
        // Liquidation ratio should be less than collateral ratio.
        if params.collateral_ratio <= params.liquidation_ratio {
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
        // Transaction fee should be less than or equal to 100%.
        if params.transaction_fee > one {
            return Err(Error::InvalidRatio);
        }
        // Auction duration should be at least a minute.
        if params.auction_duration_ms < 60_000 {
            return Err(Error::InvalidAuctionDuration);
        }
        // Auction duration should be short enough to keep liquidations recoverable.
        if params.auction_duration_ms > MAX_AUCTION_DURATION_MS {
            return Err(Error::InvalidAuctionDuration);
        }
        if params.max_oracle_age_ms == 0 {
            return Err(Error::InvalidOracleMaxAge);
        }
        Ok(())
    }
}
