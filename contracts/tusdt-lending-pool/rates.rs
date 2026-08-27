use super::*;
use ink::codegen::Env as _;

    // Ratio arithmetic helpers (Ratio doesn't impl checked_add/checked_sub directly)
    fn ratio_add(a: Ratio, b: Ratio) -> Option<Ratio> {
        Some(Ratio::from_inner(a.into_inner().checked_add(b.into_inner())?))
    }
    fn ratio_sub(a: Ratio, b: Ratio) -> Option<Ratio> {
        Some(Ratio::from_inner(a.into_inner().checked_sub(b.into_inner())?))
    }

/// Fixed-point division helper: (num * 1e18) / denom.
/// Safe because both ≤ 1e18, so num * 1e18 ≤ 1e36 < u128::MAX (~3.4e38).
fn div_ratio(num: Ratio, denom: Ratio) -> Option<Ratio> {
    let num_inner = num.into_inner();
    let denom_inner = denom.into_inner();
    Some(Ratio::from_inner(
        num_inner.checked_mul(Ratio::one().into_inner())?.checked_div(denom_inner)?,
    ))
}

impl TusdtLendingPool {
        // ─────────────────────────────────────────────────────────────
        // Interest rate math
        // ─────────────────────────────────────────────────────────────

        /// Accrues interest for a supply/borrow market (0 = TAO, 1 = TUSDT); a
        /// no-op for alpha markets (id >= 2). Updates borrow index, exchange rate,
        /// and reserve, and emits `MarketAccrued`. Charges only whole elapsed
        /// hours and advances `last_update` by whole hours so the sub-hour
        /// remainder is preserved (vault pattern). Errors:
        /// `Error::MarketNotFound`, `Error::ArithmeticError`.
        pub(crate) fn accrue_interest(&mut self, market_id: u8) -> Result<()> {
            if market_id >= 2 {
                return Ok(());
            }
            let now = self.env().block_timestamp();
            let mut state = self.markets.get(market_id).ok_or(Error::MarketNotFound)?;
            let dt_ms = now.checked_sub(state.last_update).ok_or(Error::ArithmeticError)?;
            let dt_hours = dt_ms / tusdt_primitives::MILLISECONDS_PER_HOUR;
            if state.total_scaled_debt == 0 {
                // Nothing to accrue; keep the clock on real time. There is no
                // debt whose sub-hour remainder could be starved.
                if dt_ms > 0 {
                    state.last_update = now;
                    self.markets.insert(market_id, &state);
                }
                return Ok(());
            }
            if dt_hours == 0 {
                // Live debt but less than one full hour elapsed: leave
                // `last_update` untouched so the partial hour carries into
                // the next accrual window instead of being discarded
                // (vault pattern — whole-hours-only advance).
                return Ok(());
            }
            let cash = self.market_cash(market_id)?;
            let total_liquidity =
                state.total_debt.checked_add(cash).ok_or(Error::ArithmeticError)?;
            let utilization = if total_liquidity == 0 {
                Ratio::from_inner(0)
            } else {
                Ratio::from_integer(state.total_debt.into())
                    .checked_div_int(total_liquidity.into())
                    .ok_or(Error::ArithmeticError)?
            };
            let params = self.market_params.get(market_id).ok_or(Error::MarketNotFound)?;
            let borrow_rate_annual = Self::compute_borrow_rate(&params, utilization)?;
            let one = Ratio::one();
            // checked_div_int expects a RAW integer divisor, not a Ratio inner:
            // passing hours_per_year.into_inner() (8760 × 1e18) double-scales
            // and overflows u128 → ArithmeticError on every accrual.
            let hours_per_year = tusdt_primitives::HOURS_PER_YEAR;
            let borrow_rate_hourly = borrow_rate_annual
                .checked_div_int(hours_per_year)
                .ok_or(Error::ArithmeticError)?;
            let one_minus_rf =
                ratio_sub(one, params.reserve_factor).ok_or(Error::ArithmeticError)?;
            let supply_rate_annual = borrow_rate_annual
                .checked_mul(utilization)
                .and_then(|r| r.checked_mul(one_minus_rf))
                .ok_or(Error::ArithmeticError)?;
            let supply_rate_hourly = supply_rate_annual
                .checked_div_int(hours_per_year)
                .ok_or(Error::ArithmeticError)?;
            let borrow_growth = ratio_add(one, borrow_rate_hourly)
                .and_then(|f| f.checked_pow(dt_hours.into()))
                .ok_or(Error::ArithmeticError)?;
            let supply_growth = ratio_add(one, supply_rate_hourly)
                .and_then(|f| f.checked_pow(dt_hours.into()))
                .ok_or(Error::ArithmeticError)?;
            // Scaled-total accounting: the face total is derived from the
            // scaled total at the NEW borrow index. Compounding a stale floored
            // integer (the previous approach) drifted the total away from the
            // sum of per-user `scaled_debt × borrow_index` floors — the root
            // cause of unrepayable dust. The scaled total is never mutated
            // here: interest accrues purely through index growth.
            let new_borrow_index =
                state.borrow_index.checked_mul(borrow_growth).ok_or(Error::ArithmeticError)?;
            let debt_before = state.total_debt;
            let new_debt =
                scaled_debt_to_face(state.total_scaled_debt, new_borrow_index)
                    .ok_or(Error::ArithmeticError)?;
            let debt_interest = new_debt.checked_sub(debt_before).ok_or(Error::ArithmeticError)?;
            let new_exchange_rate =
                state.exchange_rate.checked_mul(supply_growth).ok_or(Error::ArithmeticError)?;
            let supply_interest = ratio_sub(new_exchange_rate, state.exchange_rate)
                .and_then(|g| g.checked_mul_value(state.total_supplied.into()))
                .and_then(|v| Balance::try_from(v).ok())
                .unwrap_or(0);
            let reserve_delta = debt_interest.saturating_sub(supply_interest);
            state.total_debt = new_debt;
            state.borrow_index = new_borrow_index;
            state.exchange_rate = new_exchange_rate;
            state.reserve_accrued =
                state.reserve_accrued.checked_add(reserve_delta).ok_or(Error::ArithmeticError)?;
            // Advance by whole hours only (vault pattern): the sub-hour
            // remainder carries into the next accrual window instead of
            // being discarded.
            state.last_update = state
                .last_update
                .checked_add(
                    dt_hours
                        .checked_mul(tusdt_primitives::MILLISECONDS_PER_HOUR)
                        .ok_or(Error::ArithmeticError)?,
                )
                .ok_or(Error::ArithmeticError)?;
            self.markets.insert(market_id, &state);
            self.env().emit_event(MarketAccrued {
                market: market_id,
                dt_hours,
                utilization: utilization.into_inner(),
                borrow_rate: borrow_rate_annual.into_inner(),
                supply_rate: supply_rate_annual.into_inner(),
                reserve_delta,
            });
            Ok(())
        }

        /// Charges a new borrow one hour of interest up front — the vault's
        /// "charge at the hour beginning" model. The returned premium is added
        /// to the borrowed amount before it is converted to scaled debt, so the
        /// borrower's debt shows accrued interest from the instant of the
        /// borrow instead of staying flat until the market crosses its next
        /// whole-hour boundary. Their next increase then arrives with the
        /// market's next hourly accrual.
        ///
        /// Why the premium is priced here and not in `accrue_interest`: the
        /// pool's borrow index is GLOBAL, so an accrual cannot single out a
        /// freshly-opened position. Charging `dt_hours + 1` there would bill
        /// every existing borrower an extra hour on every call and compound it
        /// repeatedly. Pricing the premium into the new position's scaled debt
        /// is the only way to bill exactly the new borrower.
        ///
        /// The premium is split exactly like index-driven interest:
        /// `reserve_factor` to `reserve_accrued`, the remainder to suppliers by
        /// growing `exchange_rate`. That split has to happen here because a
        /// scaled-debt bump is invisible to `accrue_interest`'s
        /// `new_debt - debt_before` diff, so the value would otherwise fall
        /// through to residual pool cash and reach nobody. With nothing
        /// supplied there is no exchange rate to grow and the whole premium
        /// goes to the reserve.
        ///
        /// The rate is taken at the utilization the market will have AFTER this
        /// borrow, since that is the rate the prepaid hour represents. The
        /// utilization denominator is unchanged by a borrow (debt rises by
        /// exactly what cash falls by), so only the numerator moves.
        ///
        /// Errors: `Error::MarketNotFound`, `Error::ArithmeticError`.
        pub(crate) fn charge_prepaid_hour(
            &self,
            market_id: u8,
            state: &mut MarketState,
            amount: Balance,
        ) -> Result<Balance> {
            let params = self.market_params.get(market_id).ok_or(Error::MarketNotFound)?;
            let cash = self.market_cash(market_id)?;
            let total_liquidity =
                state.total_debt.checked_add(cash).ok_or(Error::ArithmeticError)?;
            let projected_debt =
                state.total_debt.checked_add(amount).ok_or(Error::ArithmeticError)?;
            let utilization = if total_liquidity == 0 {
                Ratio::from_inner(0)
            } else {
                Ratio::from_integer(projected_debt.into())
                    .checked_div_int(total_liquidity.into())
                    .ok_or(Error::ArithmeticError)?
            };
            let borrow_rate_annual = Self::compute_borrow_rate(&params, utilization)?;
            let borrow_rate_hourly = borrow_rate_annual
                .checked_div_int(tusdt_primitives::HOURS_PER_YEAR)
                .ok_or(Error::ArithmeticError)?;
            let premium = borrow_rate_hourly
                .checked_mul_value(amount.into())
                .and_then(|v| Balance::try_from(v).ok())
                .ok_or(Error::ArithmeticError)?;
            if premium == 0 {
                // Dust borrow (or a zero rate): nothing to charge or split.
                return Ok(0);
            }
            let supply_share = if state.total_supplied == 0 {
                0
            } else {
                ratio_sub(Ratio::one(), params.reserve_factor)
                    .and_then(|r| r.checked_mul_value(premium.into()))
                    .and_then(|v| Balance::try_from(v).ok())
                    .ok_or(Error::ArithmeticError)?
            };
            let reserve_share =
                premium.checked_sub(supply_share).ok_or(Error::ArithmeticError)?;
            if supply_share > 0 {
                // Face value owed to suppliers is `total_supplied × exchange_rate`,
                // so crediting `supply_share` of face value means growing the rate
                // by `supply_share / total_supplied` (the same relation
                // `accrue_interest` inverts to derive `supply_interest`).
                let delta = Ratio::from_integer(supply_share.into())
                    .checked_div_int(state.total_supplied.into())
                    .ok_or(Error::ArithmeticError)?;
                state.exchange_rate =
                    ratio_add(state.exchange_rate, delta).ok_or(Error::ArithmeticError)?;
            }
            state.reserve_accrued =
                state.reserve_accrued.checked_add(reserve_share).ok_or(Error::ArithmeticError)?;
            Ok(premium)
        }

        /// Computes the annual borrow rate (1e18 ratio) for a given utilization
        /// from the market's interest-rate curve: `base_rate + slope1 * min(util,
        /// optimal)/optimal` plus `slope2` applied to the excess above optimal.
        /// Errors: `Error::ArithmeticError`.
        pub(crate) fn compute_borrow_rate(
            params: &InterestRateParams,
            utilization: Ratio,
        ) -> Result<Ratio> {
            if utilization.is_zero() {
                return Ok(params.base_rate);
            }
            let one = Ratio::one();
            if utilization <= params.optimal_utilization {
                let fraction = div_ratio(utilization, params.optimal_utilization)
                    .ok_or(Error::ArithmeticError)?;
                let term = params.slope1.checked_mul(fraction).ok_or(Error::ArithmeticError)?;
                ratio_add(params.base_rate, term).ok_or(Error::ArithmeticError)
            } else {
                let range =
                    ratio_sub(one, params.optimal_utilization).ok_or(Error::ArithmeticError)?;
                let excess = ratio_sub(utilization, params.optimal_utilization)
                    .ok_or(Error::ArithmeticError)?;
                let fraction = div_ratio(excess, range).ok_or(Error::ArithmeticError)?;
                let term = params.slope2.checked_mul(fraction).ok_or(Error::ArithmeticError)?;
                ratio_add(params.base_rate, params.slope1)
                    .and_then(|r| ratio_add(r, term))
                    .ok_or(Error::ArithmeticError)
            }
        }

        /// Returns the pool's physical cash for a market: the native balance
        /// plus any TAO staked on the root subnet for TAO (market 0) and the
        /// pool's TUSDT balance for market 1. Root stake is 1:1 TAO with
        /// synchronous unstake, so it counts as cash for liquidity checks and
        /// the utilization denominator. Errors: `Error::MarketNotFound` for
        /// any other market, `Error::ArithmeticError` on overflow.
        pub(crate) fn market_cash(&self, market_id: u8) -> Result<Balance> {
            match market_id {
                0 => self
                    .env()
                    .balance()
                    .checked_add(self.staked_tao)
                    .ok_or(Error::ArithmeticError),
                1 => Ok(self.tusdt.balance_of(self.env().account_id())),
                _ => Err(Error::MarketNotFound),
            }
        }
}
