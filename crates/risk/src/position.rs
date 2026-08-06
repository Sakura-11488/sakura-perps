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

/// Whether a price is being struck to open a position or to close one.
///
/// The direction matters because the adverse side flips: opening a long and
/// closing a short both pay *up*; opening a short and closing a long both
/// receive *down*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriceDirection {
    /// Striking the entry price of a new position.
    Open,
    /// Striking the exit price of an existing one.
    Close,
}

/// The price a trade actually executes at — never better for the trader than
/// `mid`.
///
/// Two adjustments, both applied against the trader:
///
/// * **Confidence.** The oracle publishes an interval, not a point. Treating the
///   midpoint as truth hands the trader the benefit of that uncertainty; taking
///   the adverse edge means a wide interval costs the person who chose to trade
///   through it rather than the pool.
/// * **Spread.** The venue's own charge for immediacy.
///
/// Rounding follows the house rule: up when the trader pays, down when the
/// trader receives.
pub fn execution_price(
    side: Side,
    direction: PriceDirection,
    mid: u128,
    confidence: u128,
    spread_bps: u16,
) -> Result<u128, RiskError> {
    if mid == 0 {
        return Err(RiskError::InvalidPrice);
    }
    if spread_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }

    // A long pays on the way in and receives on the way out; a short mirrors it.
    // `pays_up` is true exactly when the adverse direction is upward.
    let pays_up = matches!(
        (side, direction),
        (Side::Long, PriceDirection::Open) | (Side::Short, PriceDirection::Close)
    );

    let spread = if pays_up {
        mul_div_ceil(mid, spread_bps as u128, BPS_DENOMINATOR)?
    } else {
        mul_div_floor(mid, spread_bps as u128, BPS_DENOMINATOR)?
    };

    if pays_up {
        mid.checked_add(confidence)
            .and_then(|p| p.checked_add(spread))
            .ok_or(RiskError::MathOverflow)
    } else {
        let adverse = confidence
            .checked_add(spread)
            .ok_or(RiskError::MathOverflow)?;
        // Never zero or negative: a zero execution price makes notional zero and
        // the position unvaluable, which is worse than refusing the trade.
        if adverse >= mid {
            return Err(RiskError::InvalidPrice);
        }
        Ok(mid - adverse)
    }
}

/// Fee on a notional, in basis points, rounded **up** — the pool never
/// under-collects.
pub fn trade_fee(notional_usd: u128, fee_bps: u16) -> Result<u128, RiskError> {
    if fee_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    mul_div_ceil(notional_usd, fee_bps as u128, BPS_DENOMINATOR)
}

/// How a collected fee divides between the protocol and the liquidity pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeSplit {
    /// The protocol treasury's share, floored.
    pub protocol_usd: u128,
    /// The liquidity pool's share, which absorbs the rounding remainder.
    pub lp_usd: u128,
}

/// Split a fee, with the remainder going to the **pool**.
///
/// The protocol share is floored and the pool takes the rest, so the two parts
/// re-sum to exactly `fee_usd` — no dust is minted, and none is lost.
pub fn fee_split(fee_usd: u128, protocol_share_bps: u16) -> Result<FeeSplit, RiskError> {
    if protocol_share_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    let protocol_usd = mul_div_floor(fee_usd, protocol_share_bps as u128, BPS_DENOMINATOR)?;
    let lp_usd = fee_usd
        .checked_sub(protocol_usd)
        .ok_or(RiskError::MathOverflow)?;
    Ok(FeeSplit {
        protocol_usd,
        lp_usd,
    })
}

/// The most the pool will ever pay a position, as a fraction of its **entry
/// notional** — floored.
///
/// Of entry notional rather than of collateral: the cap bounds what the pool
/// underwrites, and collateral is the trader's own money. Snapshotting it per
/// position at open is what lets the reserve be exact.
pub fn profit_cap_usd(entry_notional_usd: u128, max_profit_bps: u16) -> Result<u128, RiskError> {
    if max_profit_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    mul_div_floor(entry_notional_usd, max_profit_bps as u128, BPS_DENOMINATOR)
}

/// The outcome of closing a position, in collateral base units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseSettlement {
    /// Equity owed after the profit cap, before the close fee.
    pub gross_payout_quote: u128,
    /// Charged on the way out, never more than the gross payout.
    pub close_fee_quote: u128,
    /// What the owner actually receives.
    pub net_payout_quote: u128,
    /// Shortfall the pool absorbs when equity is non-positive. Recorded in M5,
    /// never socialised.
    pub bad_debt_usd: u128,
    /// Whether the profit cap bound. Worth surfacing: a capped close pays less
    /// than the equity a trader can compute for themselves.
    pub profit_capped: bool,
}

/// Resolve a close into payout, fee and bad debt.
///
/// The profit cap applies to **gross equity, before** the close fee. Applying it
/// afterwards would let the fee push a capped payout below the cap, making
/// "the fee never exceeds the payout" and "the cap binds" contradict each other.
///
/// The insolvent branch computes bad debt with an explicit `unsigned_abs`, not a
/// saturating subtraction. Saturating arithmetic is banned repo-wide, and a
/// missing clamp here would make a worthless position permanently *unclosable*
/// rather than merely worthless.
pub fn settle_close(
    collateral_quote: u128,
    reserve_quote: u128,
    equity_usd: i128,
    close_fee_usd: u128,
    collateral_decimals: u8,
) -> Result<CloseSettlement, RiskError> {
    if equity_usd <= 0 {
        return Ok(CloseSettlement {
            gross_payout_quote: 0,
            close_fee_quote: 0,
            net_payout_quote: 0,
            bad_debt_usd: equity_usd.unsigned_abs(),
            profit_capped: false,
        });
    }

    let equity_quote = crate::scale::usd_to_quote_floor(equity_usd as u128, collateral_decimals)?;
    let cap = collateral_quote
        .checked_add(reserve_quote)
        .ok_or(RiskError::MathOverflow)?;
    let profit_capped = equity_quote > cap;
    let gross_payout_quote = equity_quote.min(cap);

    // The fee cannot exceed the payout: there is nothing else to take it from,
    // and a position must never close owing money it has already surrendered.
    let close_fee_quote =
        crate::scale::usd_to_quote_ceil(close_fee_usd, collateral_decimals)?.min(gross_payout_quote);
    let net_payout_quote = gross_payout_quote
        .checked_sub(close_fee_quote)
        .ok_or(RiskError::MathOverflow)?;

    Ok(CloseSettlement {
        gross_payout_quote,
        close_fee_quote,
        net_payout_quote,
        bad_debt_usd: 0,
        profit_capped,
    })
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
