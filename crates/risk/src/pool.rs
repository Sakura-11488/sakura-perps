//! Liquidity-pool share mathematics.
//!
//! # The first-deposit attack
//!
//! The classic failure here is the ERC-4626 inflation attack, and it works on
//! Solana just as well. An attacker deposits one base unit for one share,
//! transfers a large amount directly into the vault token account, and the next
//! depositor computes `floor(deposit × 1 / huge) = 0` shares — receiving
//! nothing while their deposit inflates the attacker's single share.
//!
//! Three independent defences, all of which should be present:
//!
//! 1. **AUM is a tracked number, never `vault.amount`.** This alone defeats the
//!    donation vector, because a direct transfer into the vault does not change
//!    the pool's recorded equity. It is the reason [`shares_for_deposit`] takes
//!    `aum_usd` as an argument rather than a balance.
//! 2. **A permanently locked minimum**, [`MINIMUM_LIQUIDITY`], minted to the
//!    pool itself on the first deposit and never withdrawable. Uniswap v2's
//!    approach; it makes the share price impossible to manipulate by orders of
//!    magnitude with a dust deposit.
//! 3. **A mandatory minimum output**, checked by the caller, plus the
//!    zero-shares rejection below.

use crate::error::RiskError;
use crate::math::{mul_div_ceil, mul_div_floor};
use crate::scale::BPS_DENOMINATOR;

/// Shares minted to the pool itself on first deposit and never redeemable.
///
/// One whole share at six decimals. The cost is borne once by whoever seeds the
/// pool, and it permanently bounds how far the share price can be moved by a
/// small deposit.
pub const MINIMUM_LIQUIDITY: u128 = 1_000_000;

/// Assets under management and, if the pool is insolvent, by how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aum {
    /// Assets available to back shares. Zero when the pool is insolvent.
    pub value_usd: u128,
    /// Amount by which trader liabilities exceed pool equity. Zero when solvent.
    pub shortfall_usd: u128,
}

impl Aum {
    /// Whether liabilities exceed equity.
    pub fn is_insolvent(&self) -> bool {
        self.shortfall_usd > 0
    }
}

/// Assets under management, from tracked equity minus what the pool owes
/// traders in unrealised profit.
///
/// Trader profit is the pool's liability, so it must be subtracted before any
/// share is priced. A pool that ignores it lets a departing LP withdraw at a
/// price that does not account for money already promised to open positions.
///
/// Insolvency is reported, not clamped. Returning a bare `0` for "liabilities
/// exceed equity" would be indistinguishable from an empty pool, and the caller
/// needs to tell those apart — one is a socialised-loss event, the other is a
/// pool nobody has deposited into yet. This is exactly the reason
/// `saturating_sub` is banned in value-carrying maths: it destroys the
/// information that something went wrong.
pub fn aum_usd(pool_equity_usd: u128, trader_unrealized_profit_usd: u128) -> Aum {
    if pool_equity_usd >= trader_unrealized_profit_usd {
        Aum {
            value_usd: pool_equity_usd - trader_unrealized_profit_usd,
            shortfall_usd: 0,
        }
    } else {
        Aum {
            value_usd: 0,
            shortfall_usd: trader_unrealized_profit_usd - pool_equity_usd,
        }
    }
}

/// Shares to mint for a deposit, rounded **down**.
///
/// The first deposit sets the share price at one share per unit and permanently
/// locks [`MINIMUM_LIQUIDITY`]; the depositor receives the remainder.
///
/// Rejects a deposit that would mint zero shares. Without that check a small
/// deposit into a high-priced pool is silently confiscated.
pub fn shares_for_deposit(
    net_deposit_usd: u128,
    total_shares: u128,
    aum_usd: u128,
) -> Result<SharesMinted, RiskError> {
    if net_deposit_usd == 0 {
        return Err(RiskError::NegativeAmount);
    }

    if total_shares == 0 {
        // First deposit. Seed the price at 1:1 and burn the minimum.
        if net_deposit_usd <= MINIMUM_LIQUIDITY {
            return Err(RiskError::EmptyPool);
        }
        return Ok(SharesMinted {
            to_depositor: net_deposit_usd - MINIMUM_LIQUIDITY,
            locked_forever: MINIMUM_LIQUIDITY,
        });
    }

    if aum_usd == 0 {
        // Shares exist but the pool is worthless. Minting against this would
        // hand the depositor an arbitrary fraction of nothing, and would let the
        // next deposit be priced off a zero. Refuse.
        return Err(RiskError::EmptyPool);
    }

    let shares = mul_div_floor(net_deposit_usd, total_shares, aum_usd)?;
    if shares == 0 {
        return Err(RiskError::NegativeAmount);
    }

    Ok(SharesMinted {
        to_depositor: shares,
        locked_forever: 0,
    })
}

/// Result of [`shares_for_deposit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharesMinted {
    /// Shares credited to the depositor.
    pub to_depositor: u128,
    /// Shares minted to the pool and never redeemable. Non-zero only on the
    /// very first deposit.
    pub locked_forever: u128,
}

impl SharesMinted {
    /// Total shares minted, which is what the share supply increases by.
    pub fn total(&self) -> Result<u128, RiskError> {
        self.to_depositor
            .checked_add(self.locked_forever)
            .ok_or(RiskError::MathOverflow)
    }
}

/// Assets returned for burning shares, rounded **down**.
pub fn assets_for_shares(
    shares: u128,
    total_shares: u128,
    aum_usd: u128,
) -> Result<u128, RiskError> {
    if total_shares == 0 {
        return Err(RiskError::EmptyPool);
    }
    if shares > total_shares {
        return Err(RiskError::MathOverflow);
    }
    mul_div_floor(shares, aum_usd, total_shares)
}

/// A fee deducted from a deposit or withdrawal, rounded **up**.
///
/// These are not revenue for its own sake. A pool whose AUM jumps when a
/// liquidation settles is sandwichable: deposit immediately before, withdraw
/// immediately after. Solana has no public mempool, but Jito bundles make
/// bracketing an observed transaction entirely practical. The fee must exceed
/// the AUM movement an attacker can anticipate, which is why Jupiter varies it
/// dynamically rather than leaving it at zero.
pub fn flow_fee(amount_usd: u128, fee_bps: u16) -> Result<u128, RiskError> {
    if fee_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    mul_div_ceil(amount_usd, fee_bps as u128, BPS_DENOMINATOR)
}

/// Pool utilisation in basis points: reserved against total assets.
///
/// Returns `0` for an empty pool rather than erroring, because "nothing is
/// deployed" is the honest reading of a pool with no assets, and callers use
/// this to gate withdrawals where an error would be a denial of service.
pub fn utilization_bps(reserved_usd: u128, aum_usd: u128) -> Result<u128, RiskError> {
    if aum_usd == 0 {
        return Ok(0);
    }
    mul_div_floor(reserved_usd, BPS_DENOMINATOR, aum_usd)
}

/// Whether a withdrawal may proceed without stranding liquidity that is
/// already promised to open positions.
///
/// This is the check that stops liquidity providers front-running trader
/// profits — withdrawing precisely the assets reserved to pay them. Both GMX
/// and Jupiter enforce an equivalent.
pub fn withdrawal_leaves_enough_reserve(
    aum_after_usd: u128,
    reserved_usd: u128,
    max_utilization_bps: u16,
) -> Result<bool, RiskError> {
    if max_utilization_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    if reserved_usd == 0 {
        return Ok(true);
    }
    utilization_within_cap(reserved_usd, aum_after_usd, max_utilization_bps)
}

/// Whether `reserved / aum` sits within `max_utilization_bps`, compared as the
/// **exact rational** — `reserved × 10_000 <= max_bps × aum`.
///
/// Separate from [`withdrawal_leaves_enough_reserve`] because reserving at open
/// time asks the same question about a different quantity, and both callers must
/// get the identical answer. Comparing a *floored* utilisation instead admitted
/// an overhang of up to one basis point of AUM: `utilization_bps(20_001,
/// 20_000)` floors to 10 000 and satisfies a 10 000 ceiling despite reserving
/// more than the pool holds. The harm is not the rounding dust — it is that the
/// last position to close cannot be paid, its `checked_sub` fails, and it
/// becomes permanently unclosable.
pub fn utilization_within_cap(
    reserved_usd: u128,
    aum_usd: u128,
    max_utilization_bps: u16,
) -> Result<bool, RiskError> {
    if max_utilization_bps as u128 > BPS_DENOMINATOR {
        return Err(RiskError::InvalidBasisPoints);
    }
    if reserved_usd == 0 {
        return Ok(true);
    }
    if aum_usd == 0 {
        return Ok(false);
    }
    Ok(matches!(
        crate::math::cmp_products(
            reserved_usd,
            BPS_DENOMINATOR,
            max_utilization_bps as u128,
            aum_usd,
        ),
        core::cmp::Ordering::Less | core::cmp::Ordering::Equal
    ))
}
