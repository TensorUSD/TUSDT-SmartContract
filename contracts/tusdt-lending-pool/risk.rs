use super::*;
use ink::codegen::Env as _;

impl TusdtLendingPool {
        /// Returns the TUSDT/TAO oracle price as a 1e18 ratio, enforcing freshness
        /// against `max_oracle_age_ms`. Errors: `Error::OraclePriceUnavailable`,
        /// `Error::OraclePriceStale`, `Error::ArithmeticError`.
        pub(crate) fn get_oracle_price(&self) -> Result<Ratio> {
            let price_data = self.oracle.get_latest_price().ok_or(Error::OraclePriceUnavailable)?;
            let now = self.env().block_timestamp();
            if price_data.committed_at > now {
                return Err(Error::OraclePriceUnavailable);
            }
            let age = now.checked_sub(price_data.committed_at).ok_or(Error::ArithmeticError)?;
            if age > self.global_params.max_oracle_age_ms {
                return Err(Error::OraclePriceStale);
            }
            if price_data.price.is_zero() {
                return Err(Error::OraclePriceUnavailable);
            }
            Ok(price_data.price)
        }

        /// Returns the alpha collateral price in TUSDT per alpha unit (1e18 ratio),
        /// computed from the chain-extension alpha price (RAO) times the TUSDT/TAO
        /// oracle price. Errors: `Error::ChainExtensionFailed`, oracle errors, and
        /// `Error::ArithmeticError`.
        pub(crate) fn collateral_price(&self, netuid: u16) -> Result<Ratio> {
            let tusdt_per_tao = self.get_oracle_price()?;
            let alpha_price_rao = self
                .env()
                .extension()
                .get_alpha_price(netuid)
                .map_err(|_| Error::ChainExtensionFailed)?;
            let alpha_to_tao = Self::alpha_price_rao_to_ratio(alpha_price_rao)?;
            tusdt_per_tao.checked_mul(alpha_to_tao).ok_or(Error::ArithmeticError)
        }

        /// Converts an alpha price in RAO (1e9 per alpha) to a 1e18 ratio. Errors:
        /// `Error::ArithmeticError` on overflow.
        pub(crate) fn alpha_price_rao_to_ratio(alpha_price_rao: u64) -> Result<Ratio> {
            Ratio::from_integer(alpha_price_rao.into())
                .checked_div_int(1_000_000_000u128)
                .ok_or(Error::ArithmeticError)
        }

        /// Computes a user's effective alpha collateral for a netuid. With no
        /// yield index the effective amount equals the principal. Errors:
        /// `Error::UnapprovedNetuid`, `Error::ArithmeticError`.
        pub(crate) fn effective_alpha(&self, user: AccountId, netuid: u16) -> Result<Balance> {
            let market_id = self.netuid_to_market.get(netuid).ok_or(Error::UnapprovedNetuid)?;
            let pos = self.positions.get((market_id, user)).unwrap_or(Position {
                ltoken_balance: 0,
                scaled_debt: 0,
                alpha_principal: 0,
            });
            Ok(pos.alpha_principal)
        }

        pub(crate) fn max_liquidation_threshold_for_user(&self, user: AccountId) -> Result<Ratio> {
            let mut max_threshold = Ratio::from_inner(0);
            let count = self.market_keys.len();
            for i in 0..count {
                let market_id = self.market_keys.get(i).ok_or(Error::MarketNotFound)?;
                if market_id < 2 {
                    continue;
                }
                let netuid = self.market_to_netuid.get(market_id).ok_or(Error::MarketNotFound)?;
                let pos = self.positions.get((market_id, user)).unwrap_or(Position {
                    ltoken_balance: 0,
                    scaled_debt: 0,
                    alpha_principal: 0,
                });
                if pos.alpha_principal == 0 {
                    continue;
                }
                let params = self.alpha_params.get(netuid).unwrap_or(default_alpha_params());
                if params.liquidation_threshold.into_inner() > max_threshold.into_inner() {
                    max_threshold = params.liquidation_threshold;
                }
            }
            if max_threshold.is_zero() {
                return Ok(Ratio::from_basis_points(6000));
            }
            Ok(max_threshold)
        }

        pub(crate) fn min_collateral_factor_for_user(&self, user: AccountId) -> Result<Ratio> {
            let mut min_factor = Ratio::one();
            let count = self.market_keys.len();
            let mut found = false;
            for i in 0..count {
                let market_id = self.market_keys.get(i).ok_or(Error::MarketNotFound)?;
                if market_id < 2 {
                    continue;
                }
                let netuid = self.market_to_netuid.get(market_id).ok_or(Error::MarketNotFound)?;
                let pos = self.positions.get((market_id, user)).unwrap_or(Position {
                    ltoken_balance: 0,
                    scaled_debt: 0,
                    alpha_principal: 0,
                });
                if pos.alpha_principal == 0 {
                    continue;
                }
                found = true;
                let params = self.alpha_params.get(netuid).unwrap_or(default_alpha_params());
                if params.collateral_factor.into_inner() < min_factor.into_inner() {
                    min_factor = params.collateral_factor;
                }
            }
            if !found {
                return Ok(Ratio::from_inner(0));
            }
            Ok(min_factor)
        }

    /// Splits a full-position liquidation into the liquidator's payment and
    /// the platform's share of the seized collateral.
    ///
    /// The liquidator always pays the borrower's ENTIRE debt `D` (both debt
    /// markets, with accrued interest), so every liquidation fully clears the
    /// borrower and the pool never books a deficit:
    /// - `C > D` (collateral value exceeds the debt value): the platform
    ///   takes `min(fee, (C−D)/C)` of the collateral alpha — the fee, capped
    ///   at the surplus share so the liquidator keeps collateral worth at
    ///   least the debt paid (profit = surplus, never a loss).
    /// - `C <= D` (underwater): the platform takes nothing (no surplus to
    ///   tax) and the liquidator receives the ENTIRE collateral alpha,
    ///   accepting that it may be worth less than the full debt paid.
    ///
    /// The returned `platform_share` is a 1e18 ratio applied per-netuid to
    /// the alpha principal. `payment_value` is in TUSDT value (9-decimal)
    /// units. The share depends only on `C`, `D` and the netuid's fee; the
    /// payment is the same for every netuid.
    /// Errors: `Error::ArithmeticError`.
    pub(crate) fn full_seizure_split(
        collateral_value: Balance,
        debt_value: Balance,
        fee: Ratio,
    ) -> Result<(Balance, Ratio)> {
        if collateral_value > debt_value {
            let surplus =
                collateral_value.checked_sub(debt_value).ok_or(Error::ArithmeticError)?;
            // (C − D) / C as a 1e18 ratio — checked_div_int takes the RAW
            // integer divisor (never a Ratio inner).
            let surplus_share = Ratio::from_integer(surplus.into())
                .checked_div_int(collateral_value.into())
                .ok_or(Error::ArithmeticError)?;
            let platform_share = if fee.into_inner() < surplus_share.into_inner() {
                fee
            } else {
                surplus_share
            };
            Ok((debt_value, platform_share))
        } else {
            Ok((debt_value, Ratio::from_inner(0)))
        }
    }

    /// Computes the user's total alpha collateral value in TUSDT (9-decimal
    /// units) across all approved netuids, priced at current oracle rates.
    /// Errors: `Error::MarketNotFound`, `Error::ArithmeticError`, plus
    /// oracle/chain-extension errors.
    pub fn get_collateral_value_tusdt(&self, user: AccountId) -> Result<Balance> {
        let mut total: u128 = 0;
        let count = self.market_keys.len();
        for i in 0..count {
            let market_id = self.market_keys.get(i).ok_or(Error::MarketNotFound)?;
            if market_id < 2 {
                continue;
            }
            let netuid = self.market_to_netuid.get(market_id).ok_or(Error::MarketNotFound)?;
            let effective = self.effective_alpha(user, netuid)?;
            if effective == 0 {
                continue;
            }
            let price = self.collateral_price(netuid)?;
            total = total
                .checked_add(
                    price.checked_mul_value(effective.into()).ok_or(Error::ArithmeticError)?,
                )
                .ok_or(Error::ArithmeticError)?;
        }
        Balance::try_from(total).map_err(|_| Error::ArithmeticError)
    }

    /// Computes the user's total debt in TUSDT (9-decimal units): TUSDT debt
    /// plus TAO debt converted at the oracle price. Errors:
    /// `Error::MarketNotFound`, `Error::ArithmeticError`,
    /// `Error::OraclePriceUnavailable`, `Error::OraclePriceStale`.
    pub fn get_debt_value_tusdt(&self, user: AccountId) -> Result<Balance> {
        let tusdt_state = self.markets.get(1).ok_or(Error::MarketNotFound)?;
        let tusdt_pos = self.positions.get((1, user)).unwrap_or(Position {
            ltoken_balance: 0,
            scaled_debt: 0,
            alpha_principal: 0,
        });
        let tusdt_debt = if tusdt_pos.scaled_debt == 0 {
            0u128
        } else {
            tusdt_state
                .borrow_index
                .checked_mul_value(tusdt_pos.scaled_debt.into())
                .ok_or(Error::ArithmeticError)?
        };
        let tao_state = self.markets.get(0).ok_or(Error::MarketNotFound)?;
        let tao_pos = self.positions.get((0, user)).unwrap_or(Position {
            ltoken_balance: 0,
            scaled_debt: 0,
            alpha_principal: 0,
        });
        let tao_debt = if tao_pos.scaled_debt == 0 {
            0u128
        } else {
            tao_state
                .borrow_index
                .checked_mul_value(tao_pos.scaled_debt.into())
                .ok_or(Error::ArithmeticError)?
        };
        let tao_debt_tusdt = if tao_debt > 0 {
            self.get_oracle_price()?
                .checked_mul_value(tao_debt)
                .ok_or(Error::ArithmeticError)?
        } else {
            0
        };
        Balance::try_from(tusdt_debt.checked_add(tao_debt_tusdt).ok_or(Error::ArithmeticError)?)
            .map_err(|_| Error::ArithmeticError)
    }

    /// Returns the user's health factor as
    /// `(max liquidation threshold * collateral value) / debt value`, or `None`
    /// when the user has no debt. Errors: `Error::MarketNotFound`,
    /// `Error::ArithmeticError`, plus oracle/chain-extension errors.
    pub fn get_health_factor(&self, user: AccountId) -> Result<Option<Ratio>> {
        let debt_value = self.get_debt_value_tusdt(user)?;
        if debt_value == 0 {
            return Ok(None);
        }
        let collateral_value = self.get_collateral_value_tusdt(user)?;
        if collateral_value == 0 {
            return Ok(Some(Ratio::from_inner(0)));
        }
        let threshold = self.max_liquidation_threshold_for_user(user)?;
        let health = health_factor_from_values(threshold, collateral_value, debt_value)
            .ok_or(Error::ArithmeticError)?;
        Ok(Some(health))
    }

    /// Returns the maximum additional TUSDT the user can borrow:
    /// `min collateral factor * collateral value - current debt`. Errors:
    /// `Error::MarketNotFound`, `Error::ArithmeticError`, plus oracle/
    /// chain-extension errors.
    pub fn get_available_borrow_tusdt(&self, user: AccountId) -> Result<Balance> {
        let collateral_value = self.get_collateral_value_tusdt(user)?;
        if collateral_value == 0 {
            return Ok(0);
        }
        let debt_value = self.get_debt_value_tusdt(user)?;
        let factor = self.min_collateral_factor_for_user(user)?;
        let max_borrow = factor
            .checked_mul_value(collateral_value.into())
            .and_then(|v| Balance::try_from(v).ok())
            .ok_or(Error::ArithmeticError)?;
        if max_borrow <= debt_value {
            return Ok(0);
        }
        max_borrow.checked_sub(debt_value).ok_or(Error::ArithmeticError)
    }

    /// Returns `true` when the user's health factor is below 1.0 (underwater).
    /// Errors: `Error::MarketNotFound`, `Error::ArithmeticError`, plus
    /// oracle/chain-extension errors.
    pub fn is_liquidatable(&self, user: AccountId) -> Result<bool> {
        match self.get_health_factor(user)? {
            None => Ok(false),
            Some(hf) => Ok(hf.into_inner() < Ratio::one().into_inner()),
        }
    }
}
