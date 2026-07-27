//! Position valuation: notional, PnL, equity, margin ratio, liquidation price.

use crate::error::RiskError;
use crate::math::{ceil_div, mul_div_ceil, mul_div_floor, pow10};
use crate::scale::{BPS_DENOMINATOR, PRICE_SCALE, USD_SCALE};

/// Which way a position is facing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Profits when the price rises.
    Long,
    /// Profits when the price falls.
    Short,
}

/// The conversion constant between `size × price` and a USD value.
///
/// `notional_usd = size_base × price / (10^decimals × PRICE_SCALE / USD_SCALE)`.
/// Factored out because getting it wrong is a silent error of exactly four
/// orders of magnitude, and because it is the one place `10^decimals` appears.
fn size_price_divisor(asset_decimals: u8) -> Result<u128, RiskError> {
    let decimal_factor = pow10(asset_decimals as u32)?;
    let scale_ratio = PRICE_SCALE / USD_SCALE;
    decimal_factor
        .checked_mul(scale_ratio)
        .ok_or(RiskError::MathOverflow)
}

/// Notional value of a position in USD, rounded **down**.
///
/// Use for values credited to a user or displayed. For anything a margin
/// requirement is computed from, use [`notional_usd_ceil`].
pub fn notional_usd(size_base: u128, price: u128, asset_decimals: u8) -> Result<u128, RiskError> {
    if price == 0 {
        return Err(RiskError::InvalidPrice);
    }
    mul_div_floor(size_base, price, size_price_divisor(asset_decimals)?)
}

/// Notional value of a position in USD, rounded **up**.
///
/// Use wherever a larger notional is the conservative answer: margin
/// requirements, open-interest caps, fee bases.
pub fn notional_usd_ceil(
    size_base: u128,
    price: u128,
    asset_decimals: u8,
) -> Result<u128, RiskError> {
    if price == 0 {
        return Err(RiskError::InvalidPrice);
    }
    mul_div_ceil(size_base, price, size_price_divisor(asset_decimals)?)
}

/// Unrealised profit or loss in USD, signed.
///
/// Positive means the pool owes the trader. Rounding follows the pool's
/// interest in both directions: a gain is rounded down, a loss is rounded up in
/// magnitude.
///
/// Note the deliberate widening before subtraction. `price` and `entry_price`
/// are both `u128`; computing `price - entry_price` in the unsigned domain
/// underflows for a losing long, and with `overflow-checks` on that is a panic
/// rather than a wrong answer — but it is still a bug. The difference is taken
/// as `i128` from the start.
pub fn unrealized_pnl(
    side: Side,
    size_base: u128,
    entry_price: u128,
    price: u128,
    asset_decimals: u8,
) -> Result<i128, RiskError> {
    if price == 0 || entry_price == 0 {
        return Err(RiskError::InvalidPrice);
    }
    if size_base == 0 {
        return Ok(0);
    }

    let divisor = size_price_divisor(asset_decimals)?;

    // Work out the magnitude of the move and whether it favours the trader,
    // then round according to who benefits.
    let (gap, in_profit) = match side {
        Side::Long => {
            if price >= entry_price {
                (price - entry_price, true)
            } else {
                (entry_price - price, false)
            }
        }
        Side::Short => {
            if entry_price >= price {
                (entry_price - price, true)
            } else {
                (price - entry_price, false)
            }
        }
    };

    let magnitude = if in_profit {
        // The pool pays: round down.
        mul_div_floor(size_base, gap, divisor)?
    } else {
        // The pool receives: round up.
        mul_div_ceil(size_base, gap, divisor)?
    };

    let magnitude = i128::try_from(magnitude).map_err(|_| RiskError::MathOverflow)?;
    Ok(if in_profit { magnitude } else { -magnitude })
}

/// Account equity: collateral plus PnL, minus everything already owed.
///
/// May be negative — that is bad debt, and the caller must handle it rather
/// than clamping it away. Clamping here would hide insolvency from the one
/// place that can see it.
pub fn equity(
    collateral_usd: u128,
    pnl_usd: i128,
    funding_owed_usd: i128,
    borrow_owed_usd: u128,
) -> Result<i128, RiskError> {
    let collateral = i128::try_from(collateral_usd).map_err(|_| RiskError::MathOverflow)?;
    let borrow = i128::try_from(borrow_owed_usd).map_err(|_| RiskError::MathOverflow)?;

    collateral
        .checked_add(pnl_usd)
        .and_then(|value| value.checked_sub(funding_owed_usd))
        .and_then(|value| value.checked_sub(borrow))
        .ok_or(RiskError::MathOverflow)
}

/// Margin requirement in USD for a given notional, rounded **up**.
pub fn margin_requirement(notional_usd: u128, margin_bps: u16) -> Result<u128, RiskError> {
    if margin_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    mul_div_ceil(notional_usd, margin_bps as u128, BPS_DENOMINATOR)
}

/// Whether a position is liquidatable at the given equity and notional.
///
/// A position sitting **exactly** on the threshold is liquidatable. Ties go to
/// the pool, and an inclusive comparison removes an entire class of
/// off-by-one disputes about whether a specific liquidation was justified.
pub fn is_liquidatable(
    equity_usd: i128,
    notional_usd: u128,
    maintenance_margin_bps: u16,
) -> Result<bool, RiskError> {
    if equity_usd < 0 {
        return Ok(true);
    }
    let requirement = margin_requirement(notional_usd, maintenance_margin_bps)?;
    let requirement = i128::try_from(requirement).map_err(|_| RiskError::MathOverflow)?;
    Ok(equity_usd <= requirement)
}

/// Assert that margin parameters cannot produce a position that is liquidatable
/// the instant it is opened.
///
/// If `initial <= maintenance + liquidation_fee`, a trader can open at maximum
/// leverage and immediately liquidate themselves to collect the liquidator's
/// cut. This is checked once at market configuration, not per trade.
pub fn validate_margin_parameters(
    initial_margin_bps: u16,
    maintenance_margin_bps: u16,
    liquidation_fee_bps: u16,
) -> Result<(), RiskError> {
    if [
        initial_margin_bps,
        maintenance_margin_bps,
        liquidation_fee_bps,
    ]
    .iter()
    .any(|bps| *bps as u128 > BPS_DENOMINATOR)
    {
        return Err(RiskError::InvalidBasisPoints);
    }
    let floor = (maintenance_margin_bps as u32)
        .checked_add(liquidation_fee_bps as u32)
        .ok_or(RiskError::MathOverflow)?;
    if (initial_margin_bps as u32) <= floor {
        return Err(RiskError::InvalidMarginParameters);
    }
    Ok(())
}

/// The price at which a position becomes liquidatable.
///
/// Returns `None` when the position cannot be liquidated by an adverse move —
/// a long collateralised above its full notional, or a short whose maintenance
/// requirement can never catch up. `None` means "no such price", not "zero";
/// conflating the two would render a fully collateralised position as
/// liquidatable at any price.
///
/// Derivation, for a long, writing `s = size / divisor` so notional at price
/// `P` is `P·s`:
///
/// ```text
///   equity at liquidation  = maintenance requirement
///   collateral + (P - entry)·s = P·s·m
///   P = (entry·s - collateral) / (s·(1 - m))
/// ```
///
/// and for a short, `P = (collateral + entry·s) / (s·(1 + m))`.
///
/// `collateral_usd` must already be net of fees and accrued funding — this
/// function does not know about them, and silently ignoring them would make
/// the displayed liquidation price optimistic.
///
/// Rounding is toward **earlier** liquidation (up for a long, down for a short)
/// so the number shown to a trader is never rosier than reality.
pub fn liquidation_price(
    side: Side,
    size_base: u128,
    entry_price: u128,
    collateral_usd: u128,
    maintenance_margin_bps: u16,
    asset_decimals: u8,
) -> Result<Option<u128>, RiskError> {
    if size_base == 0 {
        return Err(RiskError::ZeroSize);
    }
    if entry_price == 0 {
        return Err(RiskError::InvalidPrice);
    }
    if maintenance_margin_bps as u128 >= BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }

    let divisor = size_price_divisor(asset_decimals)?;

    // Both sides need `entry·size` and `collateral·divisor` in the same units.
    let entry_term = entry_price
        .checked_mul(size_base)
        .ok_or(RiskError::MathOverflow)?;
    let collateral_term = collateral_usd
        .checked_mul(divisor)
        .ok_or(RiskError::MathOverflow)?;

    match side {
        Side::Long => {
            if collateral_term >= entry_term {
                // Collateral covers the entire notional; no downward move
                // liquidates this position before the price reaches zero.
                return Ok(None);
            }
            let numerator = entry_term - collateral_term;
            let denominator = size_base
                .checked_mul(BPS_DENOMINATOR - maintenance_margin_bps as u128)
                .ok_or(RiskError::MathOverflow)?;
            // Round up: a higher liquidation price for a long is the pessimistic
            // answer.
            Ok(Some(mul_div_ceil(numerator, BPS_DENOMINATOR, denominator)?))
        }
        Side::Short => {
            let numerator = entry_term
                .checked_add(collateral_term)
                .ok_or(RiskError::MathOverflow)?;
            let denominator = size_base
                .checked_mul(BPS_DENOMINATOR + maintenance_margin_bps as u128)
                .ok_or(RiskError::MathOverflow)?;
            // Round down: a lower liquidation price for a short is pessimistic.
            Ok(Some(mul_div_floor(
                numerator,
                BPS_DENOMINATOR,
                denominator,
            )?))
        }
    }
}

/// Size-weighted average entry price when adding to a position.
///
/// Rounded **against** the trader — up for a long, down for a short — so
/// repeatedly adding and removing size cannot grind out a better basis than the
/// trades actually justify.
pub fn blended_entry_price(
    side: Side,
    existing_size: u128,
    existing_entry: u128,
    added_size: u128,
    added_price: u128,
) -> Result<u128, RiskError> {
    if added_price == 0 {
        return Err(RiskError::InvalidPrice);
    }
    if existing_size == 0 {
        return Ok(added_price);
    }
    if added_size == 0 {
        return Ok(existing_entry);
    }

    let total_size = existing_size
        .checked_add(added_size)
        .ok_or(RiskError::MathOverflow)?;

    // (existing_size·existing_entry + added_size·added_price) / total_size,
    // computed as two mul_divs so neither product needs to fit alone.
    let existing_part = mul_div_floor(existing_size, existing_entry, total_size)?;
    let added_part = mul_div_floor(added_size, added_price, total_size)?;
    let floor_value = existing_part
        .checked_add(added_part)
        .ok_or(RiskError::MathOverflow)?;

    Ok(match side {
        // A higher basis for a long means less profit: conservative.
        Side::Long => floor_value.checked_add(1).ok_or(RiskError::MathOverflow)?,
        Side::Short => floor_value,
    })
}

/// Liquidation fee in USD, capped at the collateral remaining.
///
/// The cap matters: without it a liquidation can compute a fee larger than the
/// account holds, and the difference becomes bad debt created by the very
/// mechanism meant to prevent it.
pub fn liquidation_fee(
    notional_usd: u128,
    liquidation_fee_bps: u16,
    collateral_remaining_usd: u128,
) -> Result<u128, RiskError> {
    if liquidation_fee_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    let fee = mul_div_ceil(notional_usd, liquidation_fee_bps as u128, BPS_DENOMINATOR)?;
    Ok(fee.min(collateral_remaining_usd))
}

/// Leverage implied by a notional and equity, in basis points.
///
/// Returns `None` for non-positive equity, where leverage is undefined rather
/// than infinite.
pub fn leverage_bps(notional_usd: u128, equity_usd: i128) -> Result<Option<u128>, RiskError> {
    if equity_usd <= 0 {
        return Ok(None);
    }
    let equity = equity_usd as u128;
    Ok(Some(ceil_div(
        notional_usd
            .checked_mul(BPS_DENOMINATOR)
            .ok_or(RiskError::MathOverflow)?,
        equity,
    )?))
}
