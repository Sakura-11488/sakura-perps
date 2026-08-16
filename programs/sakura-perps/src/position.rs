//! Open positions.
//!
//! Isolated margin, one position per owner per market, no adds. Seeds
//! `[b"position", market, owner]`, created with `init` — never
//! `init_if_needed`, which would silently overwrite a live position's
//! accounting with a new open.
//!
//! # Why so much is snapshotted
//!
//! `maintenance_margin_bps`, `liquidation_fee_bps`, `close_fee_bps` and
//! `spread_bps` are copied onto the position at open rather than read from the
//! market at close. Reading them live would mean an admin raising a market's
//! maintenance margin could make existing positions liquidatable in the same
//! transaction — a parameter change acting as a forced liquidation.
//! Snapshotting means a position is judged by the rules it was opened under.
//!
//! `spread_bps` is the sharpest case of that, and the reason it is a field
//! rather than a live read: `execution_price` refuses to strike a price at all
//! once `confidence + spread >= mid`, so a raised market spread would not
//! merely tax every open position's exit — it would *revert* the exits it could
//! no longer price, on every path including admin settlement.
//!
//! `reserve_quote` is snapshotted for a different reason: it is the single
//! authoritative number for what the pool has set aside for this position, so
//! the amount reserved at open and the cap applied at close cannot drift apart.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;
use sakura_perps_risk::funding::{borrow_owed, funding_owed_signed};
use sakura_perps_risk::math::to_u64;
use sakura_perps_risk::oracle::diverges_beyond;
use sakura_perps_risk::position::{
    apply_liquidation_fee, equity, execution_price, fee_split_quote, is_liquidatable,
    liquidation_fee, margin_requirement, notional_usd_ceil, profit_cap_usd, settle_close,
    trade_fee, unrealized_pnl, CloseSettlement, LiquidatedSettlement, PriceDirection, Side,
};
use sakura_perps_risk::scale::{quote_to_usd_floor, usd_to_quote_ceil};

use crate::pool::{assert_pool_invariants, UtilisationCheck};
use crate::{
    Exchange, Market, PauseFlags, PerpsError, Pool, QualifiedFeed, EMERGENCY_CLOSE_DELAY_SECONDS,
};

/// Which way a position is facing.
///
/// Stored as a bare `u8` rather than an enum: `InitSpace` over an enum reserves
/// space per variant, and this is a two-state field that will never grow.
pub const SIDE_LONG: u8 = 0;
/// See [`SIDE_LONG`].
pub const SIDE_SHORT: u8 = 1;

/// One trader's isolated position in one market.
#[account]
#[derive(InitSpace)]
pub struct Position {
    pub bump: u8,
    pub owner: Pubkey,
    pub market: Pubkey,
    /// [`SIDE_LONG`] or [`SIDE_SHORT`].
    pub side: u8,
    pub size_base: u64,
    /// The **execution** price, not the oracle mid — this is what the trader
    /// actually got after confidence and spread were applied against them.
    pub entry_price: u128,
    /// Notional at entry, snapshotted. The basis for funding, borrow **and**
    /// open interest, so all three agree and none of them move with the price.
    pub entry_notional_usd: u128,
    /// Collateral held, net of the open fee.
    pub collateral_quote: u64,
    /// The profit cap, snapshotted at open. The single authoritative number for
    /// what the pool has reserved against this position.
    pub reserve_quote: u64,
    /// Snapshot — raising the market's cannot force-close an open position.
    pub maintenance_margin_bps: u16,
    /// Snapshot, for the same reason.
    pub liquidation_fee_bps: u16,
    /// Snapshot, for the same reason.
    pub close_fee_bps: u16,
    pub entry_borrow_index: u128,
    pub entry_funding_index: i128,
    pub opened_ts: i64,
    /// Slot at open. Mirrors `WithdrawRequest.requested_slot`: it makes an
    /// open-and-close inside a single slot detectable even where the clock has
    /// not advanced a whole second.
    pub opened_slot: u64,
    /// Snapshot of the market's spread, for the reason in the module docs.
    ///
    /// 64 originally; taken from `_reserved` rather than appended, so
    /// `INIT_SPACE` is unchanged and no already-allocated position would need
    /// reallocating. A position written before this field existed reads `0`
    /// here, which is the zero-spread case: `execution_price` is total at
    /// `spread_bps == 0`, so the fallback cannot make an exit unpriceable.
    pub spread_bps: u16,
    pub _reserved: [u8; 62],
}

// The on-chain length is frozen. There are live devnet accounts, and Anchor has
// no migration story: a changed `INIT_SPACE` orphans every one of them rather
// than growing them. Every stage-3 field came out of `_reserved`, and this is
// the check that says so at compile time rather than in a comment.
const _: () = assert!(Position::INIT_SPACE == 240);

impl Position {
    /// Whether this position is long.
    pub fn is_long(&self) -> bool {
        self.side == SIDE_LONG
    }

    /// The risk crate's `Side`, for valuation.
    pub fn risk_side(&self) -> sakura_perps_risk::position::Side {
        if self.is_long() {
            sakura_perps_risk::position::Side::Long
        } else {
            sakura_perps_risk::position::Side::Short
        }
    }
}

/// Which of the three settlement paths closed a position.
///
/// One event carries all three because they share one ledger — §4.2 differs
/// only in the liquidation fee — and an indexer that had to join three event
/// types to answer "how did this position end" would get it wrong eventually.
/// `EmergencyClosed` is worth alerting on: it means a market was wound down.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseReason {
    /// The owner closed it themselves.
    Ordinary,
    /// An admin liquidated it under the liquidation guards.
    AdminSettled,
    /// An admin wound it down with no oracle at all.
    EmergencyClosed,
}

#[event]
pub struct PositionOpened {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub position: Pubkey,
    pub side: u8,
    pub size_base: u64,
    pub entry_price: u128,
    pub entry_notional_usd: u128,
    pub collateral_quote: u64,
    pub reserve_quote: u64,
    pub open_fee_quote: u64,
}

/// The close event for all three settlement paths.
///
/// `close_fee_quote` and `liquidation_fee_quote` are the amounts the vault
/// **retained**, not the amounts computed before clamping. Emitting the
/// pre-clamp figures would make an indexer's fee revenue disagree with the
/// pool's, which is the same confusion that made booking them a solvency bug.
#[event]
pub struct PositionClosed {
    pub market: Pubkey,
    pub owner: Pubkey,
    pub position: Pubkey,
    pub reason: CloseReason,
    pub exit_price: u128,
    pub gross_payout_quote: u64,
    pub close_fee_quote: u64,
    pub liquidation_fee_quote: u64,
    pub net_payout_quote: u64,
    /// Whether the profit cap bound. A capped close pays less than the equity a
    /// trader can compute for themselves, so it is surfaced rather than hidden.
    pub profit_capped: bool,
    /// Recorded on the market, never socialised across liquidity providers.
    pub bad_debt_usd: u128,
}

/// Arguments to [`crate::sakura_perps::open_position`].
///
/// `side` is validated rather than normalised: a caller who sends a third value
/// meant something the protocol cannot guess, and silently booking it as a short
/// because [`Position::is_long`] tests for equality with [`SIDE_LONG`] would
/// give them a position facing the wrong way with no error to read.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct OpenPositionParams {
    /// [`SIDE_LONG`] or [`SIDE_SHORT`].
    pub side: u8,
    pub size_base: u64,
    /// Gross collateral to transfer in. The open fee comes out of it, so what
    /// the position ends up holding is this minus the fee.
    pub collateral_deposited_quote: u64,
    /// The worst **execution** price the caller will accept, at `PRICE_SCALE`.
    ///
    /// One field rather than the two the specification sketches, because only
    /// one of the two is ever read and a field that is never read still looks
    /// checked. It is a ceiling for a long, which pays up, and a floor for a
    /// short, which receives down — the spec's `max_entry_price` and
    /// `min_entry_price` respectively.
    ///
    /// Zero is rejected. Slippage protection is mandatory here for the same
    /// reason `lp_deposit` makes `min_shares_out` mandatory, and a single
    /// required field is what makes a default-constructed params struct fail on
    /// **both** sides: as a ceiling zero rejects every price, but as a floor it
    /// would accept every price, so the two readings disagree about what
    /// "unset" means and only an explicit rejection settles it.
    pub limit_price: u128,
}

/// Arguments to [`crate::sakura_perps::close_position`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ClosePositionParams {
    /// The worst **execution** price the caller will accept, at `PRICE_SCALE`,
    /// mirrored for the exit: a floor for a long, which receives, and a ceiling
    /// for a short, which pays. Zero is rejected, per
    /// [`OpenPositionParams::limit_price`].
    pub limit_price: u128,
}

/// Book a fee the vault has **already retained**.
///
/// This is B1 expressed as the only door into the pool's fee accounting. The
/// bug it forecloses is not exotic: `fee_split` and [`fee_split_quote`] have the
/// same shape, so passing the USD figure a fee was *computed from* instead of
/// the base-unit figure a settlement actually *kept* type-checks. Booking the
/// pre-clamp number credits the pool money the vault never received, which
/// pushes `quote_deposited + locked_quote + pending_protocol_fees` above
/// `quote_vault.amount` — and since every value-touching instruction ends by
/// asserting exactly that inequality, the close reverts. Permanently: M5 ships
/// no keeper liquidation, so there is no second way out of a position whose
/// ordinary close is the thing that fails.
///
/// The clamped, retained amounts are `settlement.close_fee_quote`,
/// `settled.liquidation_fee_quote` and, on the open leg, `open_fee_quote`.
/// Nothing else may reach this function.
fn book_fee(pool: &mut Pool, fee_quote: u128, protocol_fee_share_bps: u16) -> Result<()> {
    if fee_quote == 0 {
        return Ok(());
    }

    let split = fee_split_quote(fee_quote, protocol_fee_share_bps)
        .map_err(crate::oracle::map_risk_error)?;
    let protocol_quote = to_u64(split.protocol_quote).map_err(crate::oracle::map_risk_error)?;
    let lp_quote = to_u64(split.lp_quote).map_err(crate::oracle::map_risk_error)?;

    pool.pending_protocol_fees = pool
        .pending_protocol_fees
        .checked_add(protocol_quote)
        .ok_or(PerpsError::MathOverflow)?;
    // The liquidity providers' share is revenue, so it lands in tracked equity
    // rather than in a pot of its own. The two parts re-sum to exactly
    // `fee_quote`, so liabilities rise by precisely what the vault kept.
    pool.quote_deposited = pool
        .quote_deposited
        .checked_add(lp_quote)
        .ok_or(PerpsError::MathOverflow)?;

    Ok(())
}

/// What a close is worth, before any ledger is touched.
///
/// Returned rather than computed three times, because `close_position`,
/// `admin_settle_position` and `emergency_close_position` differ only in how
/// they arrive at an exit price. Three copies of this arithmetic would be three
/// chances for the paths to disagree about what a position was worth, and the
/// admin paths are precisely the ones a trader cannot check.
pub struct CloseValuation {
    /// Signed, and negative means bad debt. The liquidatability gate reads it.
    pub equity_usd: i128,
    /// Notional at the exit price, ceiled: the close fee's basis. **Not** what
    /// open interest is settled against — that is entry notional.
    pub exit_notional_usd: u128,
    /// The settlement itself. Its `close_fee_quote` is the **clamped output**,
    /// which is the only close-fee figure the ledger may book.
    pub settlement: CloseSettlement,
}

/// Steps 5 to 8 of the close path, shared by all three settlement instructions.
pub(crate) fn value_close(
    position: &Position,
    market: &Market,
    collateral_decimals: u8,
    exit_price: u128,
) -> Result<CloseValuation> {
    // 5. PnL. `unrealized_pnl` is authoritative for settlement and is the only
    //    pnl function in the protocol; nothing may substitute for it.
    let pnl_usd = unrealized_pnl(
        position.risk_side(),
        u128::from(position.size_base),
        position.entry_price,
        exit_price,
        market.asset_decimals,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // 6. Funding and borrow, both charged on **entry** notional — the exposure
    //    the trader contracted for. Charging them on current notional would let
    //    the amount owed move with the price, and a trader would then choose
    //    when to settle in order to shrink it.
    let borrow_owed_usd = borrow_owed(
        position.entry_notional_usd,
        market.cum_borrow_index,
        position.entry_borrow_index,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let funding_owed_usd = funding_owed_signed(
        position.risk_side(),
        position.entry_notional_usd,
        market.cum_funding_index,
        position.entry_funding_index,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // 7. Equity. May be negative; `settle_close` handles that rather than this
    //    clamping it away, because the shortfall is the number the pool has to
    //    record.
    let equity_usd = equity(
        quote_to_usd_floor(u128::from(position.collateral_quote), collateral_decimals)
            .map_err(crate::oracle::map_risk_error)?,
        pnl_usd,
        funding_owed_usd,
        borrow_owed_usd,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // 8. The close fee, and the settlement, both bound to names.
    //
    //    `close_fee_usd` is the **input**. `settlement.close_fee_quote` is the
    //    **output**: zero whenever equity is non-positive, and otherwise
    //    `usd_to_quote_ceil(close_fee_usd).min(gross_payout_quote)`. The ledger
    //    books the output. That distinction is B1 and it is the single most
    //    important line in this file.
    let exit_notional_usd = notional_usd_ceil(
        u128::from(position.size_base),
        exit_price,
        market.asset_decimals,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let close_fee_usd = trade_fee(exit_notional_usd, position.close_fee_bps)
        .map_err(crate::oracle::map_risk_error)?;
    let settlement = settle_close(
        u128::from(position.collateral_quote),
        u128::from(position.reserve_quote),
        equity_usd,
        close_fee_usd,
        collateral_decimals,
    )
    .map_err(crate::oracle::map_risk_error)?;

    Ok(CloseValuation {
        equity_usd,
        exit_notional_usd,
        settlement,
    })
}

/// The open ledger.
///
/// A function rather than a block inside the handler because the invariant it
/// has to preserve is a property of these lines and nothing else, and a property
/// that can only be exercised through a full `Context` is a property that gets
/// tested once, on a devnet, by hand.
///
/// # Why I1 survives this
///
/// The vault rose by `collateral_deposited_quote`. Liabilities rise by
/// `collateral_after_fee` into `locked_quote` plus `open_fee_quote` split
/// between `pending_protocol_fees` and `quote_deposited` — and those two are the
/// same number, because the caller obtained the first by subtracting the second
/// from what the vault actually received. `reserved_quote` is not in the sum: it
/// is a claim against liquidity-provider equity, not a liability of its own.
fn apply_open_ledger(
    pool: &mut Pool,
    market: &mut Market,
    position: &Position,
    open_fee_quote: u64,
    protocol_fee_share_bps: u16,
) -> Result<()> {
    // The trader's money, not the liquidity providers'. `locked_quote` is a
    // separate pot inside the same vault and is never withdrawable equity.
    pool.locked_quote = pool
        .locked_quote
        .checked_add(position.collateral_quote)
        .ok_or(PerpsError::MathOverflow)?;
    market.locked_quote = market
        .locked_quote
        .checked_add(position.collateral_quote)
        .ok_or(PerpsError::MathOverflow)?;

    // B1 at the open leg: the split is applied to `open_fee_quote`, the amount
    // the vault kept, never to the USD figure it was derived from.
    book_fee(pool, u128::from(open_fee_quote), protocol_fee_share_bps)?;

    pool.reserved_quote = pool
        .reserved_quote
        .checked_add(position.reserve_quote)
        .ok_or(PerpsError::MathOverflow)?;
    market.reserved_quote = market
        .reserved_quote
        .checked_add(position.reserve_quote)
        .ok_or(PerpsError::MathOverflow)?;

    // Added at **entry** notional, which is the number the close path subtracts.
    if position.is_long() {
        market.long_oi_usd = market
            .long_oi_usd
            .checked_add(position.entry_notional_usd)
            .ok_or(PerpsError::MathOverflow)?;
        market.long_positions = market
            .long_positions
            .checked_add(1)
            .ok_or(PerpsError::MathOverflow)?;
    } else {
        market.short_oi_usd = market
            .short_oi_usd
            .checked_add(position.entry_notional_usd)
            .ok_or(PerpsError::MathOverflow)?;
        market.short_positions = market
            .short_positions
            .checked_add(1)
            .ok_or(PerpsError::MathOverflow)?;
    }

    Ok(())
}

/// The close ledger. One implementation, three callers.
///
/// The only thing that differs between an ordinary close, an admin settlement
/// and an emergency close is `settled.liquidation_fee_quote`, which is zero on
/// two of the three — so they share this body and take a
/// [`LiquidatedSettlement`] whose four base-unit fields re-sum by construction.
/// The ordinary path reaches that type by applying a zero liquidation fee, which
/// is the identity; making the type uniform is what stops a path transferring
/// `CloseSettlement::net_payout_quote` where it should transfer the liquidated
/// one.
///
/// # Why I1 survives this, exactly and without dust
///
/// The vault falls by `net = gross − close_fee_q − liq_fee_q`. Liabilities:
/// `locked_quote` falls by the collateral, `pending_protocol_fees` and
/// `quote_deposited` rise by the two fee splits — which re-sum to exactly the
/// fees, [`fee_split_quote`] having no rounding leak — and `quote_deposited`
/// absorbs `collateral − gross` in whichever direction it falls. The sum moves
/// by `−gross + close_fee_q + liq_fee_q`, the same amount the vault moved.
/// **An equality of differences, with no slack in either direction**, which is
/// why every line here has to be exact rather than merely conservative.
fn apply_close_ledger(
    pool: &mut Pool,
    market: &mut Market,
    position: &Position,
    settled: &LiquidatedSettlement,
    protocol_fee_share_bps: u16,
) -> Result<()> {
    // Trader collateral leaves the pot it was held in. `checked_sub` on the
    // market slice as well as the pool total: a market releasing more than it
    // locked is the accounting drift I3 exists to catch, and catching it here
    // names the field rather than leaving an assertion to say only that some
    // slice exceeded some total.
    pool.locked_quote = pool
        .locked_quote
        .checked_sub(position.collateral_quote)
        .ok_or(PerpsError::MathOverflow)?;
    market.locked_quote = market
        .locked_quote
        .checked_sub(position.collateral_quote)
        .ok_or(PerpsError::MathOverflow)?;

    pool.reserved_quote = pool
        .reserved_quote
        .checked_sub(position.reserve_quote)
        .ok_or(PerpsError::MathOverflow)?;
    market.reserved_quote = market
        .reserved_quote
        .checked_sub(position.reserve_quote)
        .ok_or(PerpsError::MathOverflow)?;

    // Open interest comes off at **entry** notional, which is what makes the
    // counters return to zero when every position closes. Subtracting exit
    // notional would leave a residue proportional to how far the price moved,
    // the cap would drift out of meaning, and I4 is the assertion that catches
    // it having done so.
    if position.is_long() {
        market.long_oi_usd = market
            .long_oi_usd
            .checked_sub(position.entry_notional_usd)
            .ok_or(PerpsError::MathOverflow)?;
        market.long_positions = market
            .long_positions
            .checked_sub(1)
            .ok_or(PerpsError::MathOverflow)?;
    } else {
        market.short_oi_usd = market
            .short_oi_usd
            .checked_sub(position.entry_notional_usd)
            .ok_or(PerpsError::MathOverflow)?;
        market.short_positions = market
            .short_positions
            .checked_sub(1)
            .ok_or(PerpsError::MathOverflow)?;
    }

    // Liquidity providers absorb the difference between what the trader put in
    // and what they take out. A trader loss credits them; a trader profit debits
    // them. The `checked_sub` branch can only fail if the pool's equity were
    // smaller than a payout it underwrote, which the reserve and the utilisation
    // ceiling together forbid: `gross − collateral <= position.reserve_quote` by
    // `settle_close`'s cap, and I2 bounds total reserves by a fraction of
    // `quote_deposited`.
    let gross_payout_quote =
        to_u64(settled.gross_payout_quote).map_err(crate::oracle::map_risk_error)?;
    if gross_payout_quote <= position.collateral_quote {
        pool.quote_deposited = pool
            .quote_deposited
            .checked_add(position.collateral_quote - gross_payout_quote)
            .ok_or(PerpsError::MathOverflow)?;
    } else {
        pool.quote_deposited = pool
            .quote_deposited
            .checked_sub(gross_payout_quote - position.collateral_quote)
            .ok_or(PerpsError::MathOverflow)?;
    }

    // B1. Both figures are settlement **outputs**, already clamped against the
    // payout by `settle_close` and `apply_liquidation_fee`. See [`book_fee`].
    book_fee(pool, settled.close_fee_quote, protocol_fee_share_bps)?;
    book_fee(pool, settled.liquidation_fee_quote, protocol_fee_share_bps)?;

    // Recorded on the market, never socialised across liquidity providers.
    market.cum_bad_debt_usd = market
        .cum_bad_debt_usd
        .checked_add(settled.bad_debt_usd)
        .ok_or(PerpsError::MathOverflow)?;

    Ok(())
}

/// Move a settled payout out of the vault, under the pool's own authority.
///
/// One function for the same reason [`apply_close_ledger`] is one function:
/// there are three settlement paths and one vault. Three hand-written
/// signer-seed blocks would be three chances to sign with the vault's own bump
/// rather than the pool's, or to drop the zero guard — and two of the three
/// paths are ones a position's owner never gets to inspect before they run.
fn pay_owner<'info>(
    pool: &Account<'info, Pool>,
    quote_vault: &InterfaceAccount<'info, TokenAccount>,
    collateral_mint: &InterfaceAccount<'info, Mint>,
    owner_token_account: &InterfaceAccount<'info, TokenAccount>,
    token_program: &Interface<'info, TokenInterface>,
    amount: u64,
) -> Result<()> {
    // A zero payout is legal on every path and is not the stranded-value case
    // the pool guards against elsewhere: what is being destroyed is the
    // trader's own position, which is genuinely worthless. The rent still comes
    // back through the `close = owner` constraint.
    if amount == 0 {
        return Ok(());
    }

    // The vault's authority is the **pool** PDA. `pool.vault_bump` appears only
    // in the vault's own seeds constraint and never signs anything.
    let pool_seeds: &[&[u8]] = &[b"pool", &[pool.bump]];
    let signer: &[&[&[u8]]] = &[pool_seeds];

    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(),
            TransferChecked {
                from: quote_vault.to_account_info(),
                mint: collateral_mint.to_account_info(),
                to: owner_token_account.to_account_info(),
                authority: pool.to_account_info(),
            },
            signer,
        ),
        amount,
        collateral_mint.decimals,
    )
}

/// The four base-unit amounts a settlement resolves to, narrowed once.
///
/// `LiquidatedSettlement` carries `u128`s so the risk crate can stay free of
/// the program's account widths. Narrowing them in one place means the event,
/// the transfer and the ledger cannot disagree about which of them overflowed.
struct SettledQuote {
    gross_payout_quote: u64,
    close_fee_quote: u64,
    liquidation_fee_quote: u64,
    net_payout_quote: u64,
}

impl SettledQuote {
    fn narrow(settled: &LiquidatedSettlement) -> Result<Self> {
        Ok(Self {
            gross_payout_quote: to_u64(settled.gross_payout_quote)
                .map_err(crate::oracle::map_risk_error)?,
            close_fee_quote: to_u64(settled.close_fee_quote)
                .map_err(crate::oracle::map_risk_error)?,
            liquidation_fee_quote: to_u64(settled.liquidation_fee_quote)
                .map_err(crate::oracle::map_risk_error)?,
            net_payout_quote: to_u64(settled.net_payout_quote)
                .map_err(crate::oracle::map_risk_error)?,
        })
    }
}

/// The price `emergency_close_position` settles against, and where it comes
/// from.
///
/// `market.last_good_price` is written by every successful guard-passing price
/// read — the two trading paths, the admin settlement, and the permissionless
/// `refresh_market_price`. It is read out of the `Market` account rather than
/// from an oracle, which is the whole of the design: **no price account is
/// passed to that instruction**, so no guard, no staleness and no revoked feed
/// can gate it. Loosening guards was the alternative and it is not a fix — a
/// loosened guard still fails when the oracle is *absent*, and absent is what
/// revocation, delisting and an outage all produce.
///
/// Zero means the market has never priced anything, which is reachable: a
/// market can be created, activated, traded into, and quarantined again without
/// `last_good_price` ever having been written by a build that predates it. The
/// position's own entry price is then the reference. It is not a market price,
/// but it is a price this exact position genuinely transacted at, and the
/// alternative — refusing to settle — is the trap the instruction exists to
/// avoid.
///
/// Either way the result is non-zero, which is what makes `execution_price`
/// total on that path: an entry price is struck by `execution_price` itself,
/// which never returns zero.
fn emergency_reference_price(market: &Market, position: &Position) -> u128 {
    if market.last_good_price > 0 {
        market.last_good_price
    } else {
        position.entry_price
    }
}

pub fn handle_open_position(ctx: Context<OpenPosition>, params: OpenPositionParams) -> Result<()> {
    let clock = Clock::get()?;
    let collateral_decimals = ctx.accounts.exchange.collateral_decimals;
    let protocol_fee_share_bps = ctx.accounts.exchange.protocol_fee_share_bps;

    // 1. Gates. Revocation is the `feed` account's constraint — the feed is
    //    carried by this instruction and by no exit path, which is the whole of
    //    the revocation argument: a feed the admin has stopped trusting stops
    //    new risk and never traps existing risk.
    require!(
        ctx.accounts.exchange.paused_flags & PauseFlags::OPEN_POSITION == 0,
        PerpsError::TradingPaused
    );
    require!(
        !ctx.accounts.market.is_quarantined(),
        PerpsError::MarketQuarantined
    );
    require!(
        params.side == SIDE_LONG || params.side == SIDE_SHORT,
        PerpsError::InvalidPositionSide
    );
    require!(params.limit_price > 0, PerpsError::SlippageExceeded);

    // 2. Accrue first, so both indices are current before they are snapshotted.
    //    Snapshotting a stale index would silently forgive the position every
    //    unaccrued second between the last settle and this open.
    crate::market::accrue(&mut ctx.accounts.market, &ctx.accounts.pool, &clock)?;

    // 3. Price, with divergence **rejected**. Opening is the one leg where
    //    refusing is a safe default: there is no position yet to trap, so a spot
    //    far from its own EMA simply does not get traded on. Every exit clamps
    //    instead.
    let (price, ema) = crate::oracle::load_price_and_ema(
        &ctx.accounts.price_update,
        &ctx.accounts.market.feed_id,
        &ctx.accounts.market.trading_guards(),
        &clock,
    )?;
    require!(
        !diverges_beyond(price.price, ema, ctx.accounts.market.max_divergence_bps)
            .map_err(crate::oracle::map_risk_error)?,
        PerpsError::PriceDiverged
    );
    ctx.accounts.market.last_good_price = price.price;
    ctx.accounts.market.last_good_price_ts = clock.unix_timestamp;

    // 4. Execution price: adverse to the trader on the confidence interval and
    //    on the spread, both. The oracle publishes an interval and not a point,
    //    so treating its midpoint as truth would hand the trader the benefit of
    //    the uncertainty they chose to trade through.
    let side = if params.side == SIDE_LONG {
        Side::Long
    } else {
        Side::Short
    };
    let entry_price = execution_price(
        side,
        PriceDirection::Open,
        price.price,
        price.confidence,
        ctx.accounts.market.spread_bps,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let within_bound = match side {
        Side::Long => entry_price <= params.limit_price,
        Side::Short => entry_price >= params.limit_price,
    };
    require!(within_bound, PerpsError::SlippageExceeded);

    // 5. Notional, ceiled, and **once**. This single number is the basis for the
    //    open fee, the margin requirement, the reserve, open interest, funding
    //    and borrow. Ceiling puts every one of those in the pool's favour, and
    //    using one number is what makes the close path's entry-notional
    //    subtraction return the counters to exactly zero.
    let entry_notional_usd = notional_usd_ceil(
        u128::from(params.size_base),
        entry_price,
        ctx.accounts.market.asset_decimals,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // The collateral moves before the arithmetic that depends on its size, and
    // that is a deliberate departure from the specification's step ordering. The
    // reason is Token-2022: a mint carrying a transfer-fee extension delivers
    // less than was sent, and this exchange accepts Token-2022 mints. Booking a
    // liability for the requested amount when the vault received less breaks I1
    // on the spot. So the vault balance is measured before and after, and every
    // number below is derived from what actually arrived — `lp_deposit` measures
    // for the same reason. Nothing is lost by the reorder: a failed `require!`
    // below reverts the transfer along with everything else.
    let before = ctx.accounts.quote_vault.amount;
    token_interface::transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.owner_token_account.to_account_info(),
                mint: ctx.accounts.collateral_mint.to_account_info(),
                to: ctx.accounts.quote_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        params.collateral_deposited_quote,
        ctx.accounts.collateral_mint.decimals,
    )?;
    ctx.accounts.quote_vault.reload()?;
    let collateral_deposited_quote = ctx
        .accounts
        .quote_vault
        .amount
        .checked_sub(before)
        .ok_or(PerpsError::MathOverflow)?;
    require!(collateral_deposited_quote > 0, PerpsError::ZeroAmount);

    // 6. Minimums. Dust positions are free to create and expensive to carry:
    //    each one still accrues, still reserves, and still costs an admin a
    //    transaction to settle.
    require!(
        params.size_base >= ctx.accounts.market.min_position_size_base,
        PerpsError::PositionTooSmall
    );
    require!(
        entry_notional_usd >= ctx.accounts.market.min_notional_usd,
        PerpsError::PositionTooSmall
    );
    require!(
        quote_to_usd_floor(u128::from(collateral_deposited_quote), collateral_decimals)
            .map_err(crate::oracle::map_risk_error)?
            >= ctx.accounts.market.min_collateral_usd,
        PerpsError::PositionTooSmall
    );

    // 7. The open fee, bound to a name. `open_fee_quote` is the amount the vault
    //    retains, and it is the only open-leg figure the ledger may split — B1's
    //    rule applied to this leg. The `checked_sub` failing means the trader
    //    sent less collateral than their own opening fee.
    let open_fee_usd = trade_fee(entry_notional_usd, ctx.accounts.market.open_fee_bps)
        .map_err(crate::oracle::map_risk_error)?;
    let open_fee_quote = to_u64(
        usd_to_quote_ceil(open_fee_usd, collateral_decimals)
            .map_err(crate::oracle::map_risk_error)?,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let collateral_after_fee = collateral_deposited_quote
        .checked_sub(open_fee_quote)
        .ok_or(PerpsError::MathOverflow)?;

    // 8. Initial margin, on what the fee left. Checked after the fee rather than
    //    before it, because a position that cannot pay its own opening fee and
    //    still meet margin is opening under-margined.
    require!(
        quote_to_usd_floor(u128::from(collateral_after_fee), collateral_decimals)
            .map_err(crate::oracle::map_risk_error)?
            >= margin_requirement(entry_notional_usd, ctx.accounts.market.initial_margin_bps)
                .map_err(crate::oracle::map_risk_error)?,
        PerpsError::InsufficientMargin
    );

    // 10. The reserve, ceiled: the pool must set aside at least what it may owe.
    let reserve_quote = to_u64(
        usd_to_quote_ceil(
            profit_cap_usd(entry_notional_usd, ctx.accounts.market.max_profit_bps)
                .map_err(crate::oracle::map_risk_error)?,
            collateral_decimals,
        )
        .map_err(crate::oracle::map_risk_error)?,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // 11. The open-interest cap, on the side being added to.
    let is_long = side == Side::Long;
    let side_oi_after = if is_long {
        ctx.accounts.market.long_oi_usd
    } else {
        ctx.accounts.market.short_oi_usd
    }
    .checked_add(entry_notional_usd)
    .ok_or(PerpsError::MathOverflow)?;
    require!(
        side_oi_after <= ctx.accounts.market.max_oi_usd,
        PerpsError::OpenInterestCapExceeded
    );

    let market_key = ctx.accounts.market.key();
    let owner_key = ctx.accounts.owner.key();
    let position_key = ctx.accounts.position.key();
    let side_byte = if is_long { SIDE_LONG } else { SIDE_SHORT };

    // 9. Snapshots. Every parameter a close will judge this position by is
    //    copied now, so an admin retuning the market cannot reach it. The module
    //    docs say why `spread_bps` is the sharpest case of that.
    let position = &mut ctx.accounts.position;
    position.bump = ctx.bumps.position;
    position.owner = owner_key;
    position.market = market_key;
    position.side = side_byte;
    position.size_base = params.size_base;
    position.entry_price = entry_price;
    position.entry_notional_usd = entry_notional_usd;
    position.collateral_quote = collateral_after_fee;
    position.reserve_quote = reserve_quote;
    position.maintenance_margin_bps = ctx.accounts.market.maintenance_margin_bps;
    position.liquidation_fee_bps = ctx.accounts.market.liquidation_fee_bps;
    position.close_fee_bps = ctx.accounts.market.close_fee_bps;
    position.spread_bps = ctx.accounts.market.spread_bps;
    position.entry_borrow_index = ctx.accounts.market.cum_borrow_index;
    position.entry_funding_index = ctx.accounts.market.cum_funding_index;
    position.opened_ts = clock.unix_timestamp;
    position.opened_slot = clock.slot;

    // 13. The ledger.
    apply_open_ledger(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.market,
        &ctx.accounts.position,
        open_fee_quote,
        protocol_fee_share_bps,
    )?;

    emit!(PositionOpened {
        market: market_key,
        owner: owner_key,
        position: position_key,
        side: side_byte,
        size_base: params.size_base,
        entry_price,
        entry_notional_usd,
        collateral_quote: collateral_after_fee,
        reserve_quote,
        open_fee_quote,
    });

    // 12, and the rest of 13. I2 — the utilisation ceiling this instruction is
    // the main consumer of — lives inside `assert_pool_invariants` along with
    // I1, I3 and I4, so it is asserted here rather than duplicated above. The
    // vault was reloaded when the receipt was measured and nothing has moved
    // collateral since, so no second reload is needed.
    //
    // `Ceiling`, absolutely: this is the instruction that *adds* reserve, so it
    // is one of the two that can raise utilisation and one of the two the
    // ceiling is there to stop.
    let market_ref: &Market = &ctx.accounts.market;
    assert_pool_invariants(
        &ctx.accounts.quote_vault,
        &ctx.accounts.pool,
        Some(market_ref),
        UtilisationCheck::Ceiling,
    )
}

pub fn handle_close_position(
    ctx: Context<ClosePosition>,
    params: ClosePositionParams,
) -> Result<()> {
    let clock = Clock::get()?;
    let collateral_decimals = ctx.accounts.exchange.collateral_decimals;
    let protocol_fee_share_bps = ctx.accounts.exchange.protocol_fee_share_bps;

    // 1. The pause gate, and nothing else. **No quarantine check and no
    //    revocation check**: a market that has stopped accepting new risk must
    //    still let existing risk out, and a feed the admin has stopped trusting
    //    is a reason to stop opening rather than a reason to trap what is open.
    require!(
        ctx.accounts.exchange.paused_flags & PauseFlags::CLOSE_POSITION == 0,
        PerpsError::ClosingPaused
    );
    require!(params.limit_price > 0, PerpsError::SlippageExceeded);

    // 2. Accrue, so the indices this close settles against are current.
    crate::market::accrue(&mut ctx.accounts.market, &ctx.accounts.pool, &clock)?;

    // 3. Price, **clamped** into the EMA band in both directions and never
    //    rejected. A rejection at exit is a trap that is most valuable to a
    //    manipulator at exactly the moment it would fire; and an adverse-only
    //    clamp stops the pool paying out on a manipulated price while doing
    //    nothing to stop it charging on one.
    let (price, ema) = crate::oracle::load_price_and_ema(
        &ctx.accounts.price_update,
        &ctx.accounts.market.feed_id,
        &ctx.accounts.market.trading_guards(),
        &clock,
    )?;
    //
    //    The clamp returns the confidence alongside the mid, rescaled by the
    //    same factor. Taking one without the other is the bug: the confidence
    //    gate `validate_price` enforced is a statement about the *spot*, and
    //    `execution_price` refuses to strike a price at all once `confidence +
    //    spread >= mid`. Against a clamped-down mid, a spot-scaled confidence
    //    reaches that condition on prices the gate legitimately admitted.
    let clamped =
        crate::market::clamp_to_ema_band(&price, ema, ctx.accounts.market.max_divergence_bps)?;
    let mid = clamped.mid;
    ctx.accounts.market.last_good_price = mid;
    ctx.accounts.market.last_good_price_ts = clock.unix_timestamp;

    // 4. Execution price, off the position's **snapshotted** spread and not the
    //    market's live one. Reading it live would let an admin retroactively tax
    //    every open exit and, once `confidence + spread >= mid`, revert the ones
    //    it could no longer price.
    let side = ctx.accounts.position.risk_side();
    let exit_price = execution_price(
        side,
        PriceDirection::Close,
        mid,
        clamped.confidence,
        ctx.accounts.position.spread_bps,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let within_bound = match side {
        Side::Long => exit_price >= params.limit_price,
        Side::Short => exit_price <= params.limit_price,
    };
    require!(within_bound, PerpsError::SlippageExceeded);

    // 5 to 8.
    let valuation = value_close(
        &ctx.accounts.position,
        &ctx.accounts.market,
        collateral_decimals,
        exit_price,
    )?;

    // No liquidation fee on this path. Applying a zero one is the identity, and
    // it is done rather than skipped so that this path and the admin path hand
    // the ledger the same type — which is what stops either of them transferring
    // the wrong `net_payout_quote`.
    let settled = apply_liquidation_fee(valuation.settlement, 0, collateral_decimals)
        .map_err(crate::oracle::map_risk_error)?;

    // `liquidation_fee_quote` is provably zero on this path, and it is read
    // rather than written as a literal so the event cannot go stale if the shape
    // of the close path ever changes.
    let amounts = SettledQuote::narrow(&settled)?;

    // I2's pre-state, snapshotted before the ledger runs. A settlement can only
    // lower utilisation — `reserved_quote` falls by the position's reserve `r`
    // and `quote_deposited` falls by at most `r`, because `settle_close` caps
    // the payout at collateral plus reserve — so the monotone form is the one it
    // is entitled to. Judging an exit by the ceiling instead would mean an admin
    // who lowered `max_utilization_bps` below current utilisation had reverted
    // every close in the protocol, permanently: the setter's own cap at
    // `M5_MAX_UTILIZATION_BPS` means the ceiling cannot be raised back, and
    // `lp_deposit` — the only other way utilisation falls — would be reverting
    // too.
    let utilisation_before = UtilisationCheck::NotWorsened {
        reserved_quote: ctx.accounts.pool.reserved_quote,
        quote_deposited: ctx.accounts.pool.quote_deposited,
    };

    // 9. The ledger.
    apply_close_ledger(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.market,
        &ctx.accounts.position,
        &settled,
        protocol_fee_share_bps,
    )?;

    // 10. Transfer, then announce, then assert. The order matters: I1 compares
    //     the vault balance against recorded liabilities, so asserting it before
    //     the payout has left proves nothing — the vault is still holding the
    //     money. Same shape as `lp_withdraw`.
    pay_owner(
        &ctx.accounts.pool,
        &ctx.accounts.quote_vault,
        &ctx.accounts.collateral_mint,
        &ctx.accounts.owner_token_account,
        &ctx.accounts.token_program,
        amounts.net_payout_quote,
    )?;

    emit!(PositionClosed {
        market: ctx.accounts.market.key(),
        owner: ctx.accounts.owner.key(),
        position: ctx.accounts.position.key(),
        reason: CloseReason::Ordinary,
        exit_price,
        gross_payout_quote: amounts.gross_payout_quote,
        close_fee_quote: amounts.close_fee_quote,
        liquidation_fee_quote: amounts.liquidation_fee_quote,
        net_payout_quote: amounts.net_payout_quote,
        profit_capped: settled.profit_capped,
        bad_debt_usd: settled.bad_debt_usd,
    });

    ctx.accounts.quote_vault.reload()?;
    let market_ref: &Market = &ctx.accounts.market;
    assert_pool_invariants(
        &ctx.accounts.quote_vault,
        &ctx.accounts.pool,
        Some(market_ref),
        utilisation_before,
    )
}

/// Liquidation. The only forced exit M5 ships, and the only one an admin drives
/// end to end.
pub fn handle_admin_settle_position(ctx: Context<AdminSettlePosition>) -> Result<()> {
    let clock = Clock::get()?;
    let collateral_decimals = ctx.accounts.exchange.collateral_decimals;
    let protocol_fee_share_bps = ctx.accounts.exchange.protocol_fee_share_bps;

    // 1. The pause gate, and it is `LIQUIDATE` rather than `CLOSE_POSITION`:
    //    stopping forced exits and stopping voluntary ones are different
    //    decisions, and this is the one that takes a fee off a trader who did
    //    not ask to leave. **No quarantine check and no revocation check**, for
    //    the reason the ordinary close gives: a market that has stopped
    //    accepting new risk must still be able to shed the risk it holds.
    require!(
        ctx.accounts.exchange.paused_flags & PauseFlags::LIQUIDATE == 0,
        PerpsError::LiquidationPaused
    );

    // 2. Accrue. Funding and borrow are inputs to the equity the liquidatability
    //    gate reads, so settling against stale indices would judge the position
    //    solvent on money it already owes.
    crate::market::accrue(&mut ctx.accounts.market, &ctx.accounts.pool, &clock)?;

    // 3. Price under the **liquidation** guards, which `validate_guard_ordering`
    //    permits to be the looser of the two. Refusing to liquidate is not a
    //    safe default: a position the pool cannot close is one the pool
    //    underwrites for free, and the loss grows while the guard holds. The
    //    divergence clamp is the close path's — symmetric, never a reject, and
    //    rescaling the confidence with the mid. The rescale matters more here
    //    than anywhere else: guard ordering permits the liquidation confidence
    //    gate to be the wider of the two, so a spot-scaled confidence against a
    //    clamped-down mid reaches `execution_price`'s refusal sooner on this
    //    path than on the ordinary close — and this is the path that has to work
    //    when nothing else does.
    let (price, ema) = crate::oracle::load_price_and_ema(
        &ctx.accounts.price_update,
        &ctx.accounts.market.feed_id,
        &ctx.accounts.market.liquidation_guards(),
        &clock,
    )?;
    let clamped =
        crate::market::clamp_to_ema_band(&price, ema, ctx.accounts.market.max_divergence_bps)?;
    let mid = clamped.mid;

    // Recorded as the market's last good price even though the guards it passed
    // were the looser set. The alternative — writing it only from the trading
    // paths — leaves `emergency_close_position` settling against a *staler*
    // reference rather than a tighter one, and staleness is the failure mode
    // that matters for a number with no freshness gate on it. This price is one
    // the market genuinely transacted at, clamped into its own EMA band.
    ctx.accounts.market.last_good_price = mid;
    ctx.accounts.market.last_good_price_ts = clock.unix_timestamp;

    // 4. Execution price off the position's **snapshotted** spread. The rule
    //    bites harder here than anywhere else: this is the one exit an admin
    //    controls end to end, and because the liquidation confidence gate may be
    //    the wider of the two, `execution_price`'s refusal to strike a price at
    //    all is *more* reachable on this path than on an ordinary close. A live
    //    spread would put that refusal under admin control.
    //
    //    No slippage bound. There is no caller here whose price expectation
    //    could be protected — an admin-supplied bound would only be an admin
    //    veto on a settlement the position's own numbers already justify.
    let side = ctx.accounts.position.risk_side();
    let exit_price = execution_price(
        side,
        PriceDirection::Close,
        mid,
        clamped.confidence,
        ctx.accounts.position.spread_bps,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // 5 to 8, identical to the ordinary close and shared with it, so the two
    // paths cannot disagree about what a position was worth.
    let valuation = value_close(
        &ctx.accounts.position,
        &ctx.accounts.market,
        collateral_decimals,
        exit_price,
    )?;

    // 5b. Liquidatability, at **current** notional and not entry notional. Entry
    //     notional is right for funding, borrow and open interest, which are
    //     charged on the exposure the trader contracted for; the maintenance
    //     requirement is a statement about the exposure that exists *now*, and
    //     at entry notional a short whose price has doubled would carry half its
    //     true requirement. Ties go to the pool — `is_liquidatable` compares
    //     `equity <= requirement`.
    //
    //     Struck off the clamped mid rather than the execution price, so neither
    //     the gate nor the fee below is a function of the spread this particular
    //     position happens to have snapshotted.
    let current_notional_usd = notional_usd_ceil(
        u128::from(ctx.accounts.position.size_base),
        mid,
        ctx.accounts.market.asset_decimals,
    )
    .map_err(crate::oracle::map_risk_error)?;
    require!(
        is_liquidatable(
            valuation.equity_usd,
            current_notional_usd,
            ctx.accounts.position.maintenance_margin_bps,
        )
        .map_err(crate::oracle::map_risk_error)?,
        PerpsError::PositionNotLiquidatable
    );

    // 6. The liquidation fee — B3, and two clamps in a fixed order. The first is
    //    `liquidation_fee`'s own, against the collateral that is left, so a
    //    liquidation cannot itself manufacture bad debt. The second is
    //    `apply_liquidation_fee`'s, against what the close fee left of the gross
    //    payout — close fee first, always. Without the second, the ordinary late
    //    liquidation (equity decayed to a few dollars while the fee is still
    //    computed on full notional) underflows the transfer and the position
    //    becomes permanently unliquidatable, on the only liquidation path this
    //    milestone ships.
    let collateral_remaining_usd = quote_to_usd_floor(
        u128::from(ctx.accounts.position.collateral_quote),
        collateral_decimals,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let liq_fee_usd = liquidation_fee(
        current_notional_usd,
        ctx.accounts.position.liquidation_fee_bps,
        collateral_remaining_usd,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // From here `valuation.settlement` must not be read again. Its
    // `net_payout_quote` is gross minus the close fee **only**, so transferring
    // it would pay the trader the liquidation fee as well as charging it.
    let settled = apply_liquidation_fee(valuation.settlement, liq_fee_usd, collateral_decimals)
        .map_err(crate::oracle::map_risk_error)?;
    let amounts = SettledQuote::narrow(&settled)?;

    // I2's pre-state, snapshotted before the ledger runs. A settlement can only
    // lower utilisation — `reserved_quote` falls by the position's reserve `r`
    // and `quote_deposited` falls by at most `r`, because `settle_close` caps
    // the payout at collateral plus reserve — so the monotone form is the one it
    // is entitled to. Judging an exit by the ceiling instead would mean an admin
    // who lowered `max_utilization_bps` below current utilisation had reverted
    // every close in the protocol, permanently: the setter's own cap at
    // `M5_MAX_UTILIZATION_BPS` means the ceiling cannot be raised back, and
    // `lp_deposit` — the only other way utilisation falls — would be reverting
    // too.
    let utilisation_before = UtilisationCheck::NotWorsened {
        reserved_quote: ctx.accounts.pool.reserved_quote,
        quote_deposited: ctx.accounts.pool.quote_deposited,
    };

    // 7. The ledger, which books **both** clamped fees — B1 applied twice.
    apply_close_ledger(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.market,
        &ctx.accounts.position,
        &settled,
        protocol_fee_share_bps,
    )?;

    pay_owner(
        &ctx.accounts.pool,
        &ctx.accounts.quote_vault,
        &ctx.accounts.collateral_mint,
        &ctx.accounts.owner_token_account,
        &ctx.accounts.token_program,
        amounts.net_payout_quote,
    )?;

    emit!(PositionClosed {
        market: ctx.accounts.market.key(),
        owner: ctx.accounts.position.owner,
        position: ctx.accounts.position.key(),
        reason: CloseReason::AdminSettled,
        exit_price,
        gross_payout_quote: amounts.gross_payout_quote,
        close_fee_quote: amounts.close_fee_quote,
        liquidation_fee_quote: amounts.liquidation_fee_quote,
        net_payout_quote: amounts.net_payout_quote,
        profit_capped: settled.profit_capped,
        bad_debt_usd: settled.bad_debt_usd,
    });

    ctx.accounts.quote_vault.reload()?;
    let market_ref: &Market = &ctx.accounts.market;
    assert_pool_invariants(
        &ctx.accounts.quote_vault,
        &ctx.accounts.pool,
        Some(market_ref),
        utilisation_before,
    )
}

/// Wind a position down with **no oracle at all**.
///
/// The instruction's job is to be the exit that still works when nothing else
/// does, so every one of its properties is an absence:
///
/// * **No price account.** Not looser guards — a loosened guard still fails when
///   the oracle is *absent*, and absent is what a revoked feed, a delisting and
///   an outage all produce, because nobody pushes updates to a feed nobody uses.
///   Only removing the account removes the dependency.
/// * **No feed account**, so revocation cannot reach it.
/// * **No pause gate.** A recovery path a pause can disable is not a recovery
///   path; `close_stale_escrow` and `cancel_withdraw` make the same argument in
///   `pool.rs`, and it is stronger here.
/// * **No quarantine objection** — it *requires* the quarantine.
///
/// What it is not is a cheaper exit. Funding and borrow accrue, the close fee is
/// charged on `settle_close`'s clamp, and the position's own spread is applied
/// adversely, so an admin has no reason to prefer this path and a trader has no
/// reason to hope for it.
pub fn handle_emergency_close_position(ctx: Context<EmergencyClosePosition>) -> Result<()> {
    let clock = Clock::get()?;
    let collateral_decimals = ctx.accounts.exchange.collateral_decimals;
    let protocol_fee_share_bps = ctx.accounts.exchange.protocol_fee_share_bps;

    // The delay, measured from the quarantine. Both preconditions — the market
    // is quarantined (its own constraint) and has been for a day — are public
    // and slow, so a wind-down is announced by the chain a day before any value
    // moves. A cluster clock revised backwards fails closed and postpones the
    // instruction, which is tolerable precisely because nothing moves while it
    // is postponed and the clock corrects itself.
    let quarantined_for = clock
        .unix_timestamp
        .checked_sub(ctx.accounts.market.quarantined_ts)
        .ok_or(PerpsError::MathOverflow)?;
    require!(
        quarantined_for >= EMERGENCY_CLOSE_DELAY_SECONDS,
        PerpsError::EmergencyCloseTooSoon
    );

    // Accrual reads no oracle, so the indices are as current on this path as on
    // any other and there is no reason to forgive what the position owes.
    // Forgiving it would make an emergency close cheaper than an ordinary one,
    // which is a reason to quarantine a market rather than a reason not to.
    crate::market::accrue(&mut ctx.accounts.market, &ctx.accounts.pool, &clock)?;

    // The settlement price, read from the `Market` account. See
    // [`emergency_reference_price`]. Confidence is passed as zero because there
    // is none to read — no oracle was consulted — and the spread is the
    // position's snapshot, so the exit is adverse to the trader exactly as an
    // ordinary close would be. `execution_price` is total here: the reference is
    // non-zero and `0 + spread_bps <= MAX_SPREAD_BPS < BPS_DENOMINATOR`, so the
    // one branch that can refuse a price cannot be reached.
    let reference = emergency_reference_price(&ctx.accounts.market, &ctx.accounts.position);
    let side = ctx.accounts.position.risk_side();
    let exit_price = execution_price(
        side,
        PriceDirection::Close,
        reference,
        0,
        ctx.accounts.position.spread_bps,
    )
    .map_err(crate::oracle::map_risk_error)?;

    let valuation = value_close(
        &ctx.accounts.position,
        &ctx.accounts.market,
        collateral_decimals,
        exit_price,
    )?;

    // **No liquidation fee.** This is not a liquidation: the position is not
    // being punished for its own health, it is being moved out of the way of a
    // market being retired. Applying a zero fee rather than skipping the call is
    // what keeps the ledger's argument type uniform across all three paths.
    let settled = apply_liquidation_fee(valuation.settlement, 0, collateral_decimals)
        .map_err(crate::oracle::map_risk_error)?;
    let amounts = SettledQuote::narrow(&settled)?;

    // I2's pre-state, snapshotted before the ledger runs. A settlement can only
    // lower utilisation — `reserved_quote` falls by the position's reserve `r`
    // and `quote_deposited` falls by at most `r`, because `settle_close` caps
    // the payout at collateral plus reserve — so the monotone form is the one it
    // is entitled to. Judging an exit by the ceiling instead would mean an admin
    // who lowered `max_utilization_bps` below current utilisation had reverted
    // every close in the protocol, permanently: the setter's own cap at
    // `M5_MAX_UTILIZATION_BPS` means the ceiling cannot be raised back, and
    // `lp_deposit` — the only other way utilisation falls — would be reverting
    // too.
    let utilisation_before = UtilisationCheck::NotWorsened {
        reserved_quote: ctx.accounts.pool.reserved_quote,
        quote_deposited: ctx.accounts.pool.quote_deposited,
    };

    apply_close_ledger(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.market,
        &ctx.accounts.position,
        &settled,
        protocol_fee_share_bps,
    )?;

    pay_owner(
        &ctx.accounts.pool,
        &ctx.accounts.quote_vault,
        &ctx.accounts.collateral_mint,
        &ctx.accounts.owner_token_account,
        &ctx.accounts.token_program,
        amounts.net_payout_quote,
    )?;

    // Worth alerting on: it means a market was wound down.
    emit!(PositionClosed {
        market: ctx.accounts.market.key(),
        owner: ctx.accounts.position.owner,
        position: ctx.accounts.position.key(),
        reason: CloseReason::EmergencyClosed,
        exit_price,
        gross_payout_quote: amounts.gross_payout_quote,
        close_fee_quote: amounts.close_fee_quote,
        liquidation_fee_quote: amounts.liquidation_fee_quote,
        net_payout_quote: amounts.net_payout_quote,
        profit_capped: settled.profit_capped,
        bad_debt_usd: settled.bad_debt_usd,
    });

    ctx.accounts.quote_vault.reload()?;
    let market_ref: &Market = &ctx.accounts.market;
    assert_pool_invariants(
        &ctx.accounts.quote_vault,
        &ctx.accounts.pool,
        Some(market_ref),
        utilisation_before,
    )
}

/// Accounts for [`crate::sakura_perps::open_position`].
#[derive(Accounts)]
pub struct OpenPosition<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,

    /// Revocation lives on the feed, and **only opening reads it**. The seed
    /// derivation off `market.feed_id` is the binding — there is no `has_one`
    /// that could express it, and none is needed.
    #[account(
        seeds = [b"feed", market.feed_id.as_ref()],
        bump = feed.bump,
        constraint = !feed.revoked @ PerpsError::FeedRevoked,
    )]
    pub feed: Box<Account<'info, QualifiedFeed>>,

    /// Pinned to the account the market was created against. The feed-id check
    /// inside the SDK proves the *message* is for the right feed and not that
    /// the account was written by anyone trustworthy, so this constraint is what
    /// stops a caller supplying their own price account.
    #[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
    pub price_update: Box<Account<'info, PriceUpdateV2>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    /// `init`, never `init_if_needed`. The seeds are one position per owner per
    /// market, and `init_if_needed` would silently overwrite a live position's
    /// entry price and indices with a new open — losing the old position's
    /// accounting while the pool still holds its collateral and its reserve.
    #[account(
        init,
        payer = owner,
        space = 8 + Position::INIT_SPACE,
        seeds = [b"position", market.key().as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(address = exchange.collateral_mint @ PerpsError::WrongCollateralMint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [b"quote_vault"],
        bump = pool.vault_bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_token_account.mint == exchange.collateral_mint @ PerpsError::WrongCollateralMint,
        constraint = owner_token_account.owner == owner.key() @ PerpsError::NotTokenOwner,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,

    pub system_program: Program<'info, System>,
}

/// Accounts for [`crate::sakura_perps::close_position`].
///
/// [`OpenPosition`]'s, minus `system_program`, minus the `feed` — revocation
/// must not gate closing — and with the position closed instead of created.
#[derive(Accounts)]
pub struct ClosePosition<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,

    #[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
    pub price_update: Box<Account<'info, PriceUpdateV2>>,

    /// `mut` because the closed position's rent lands here.
    #[account(mut)]
    pub owner: Signer<'info>,

    /// `has_one = market` is the constraint whose absence let a position opened
    /// in market A be closed against market B — settled at the wrong price, on
    /// the wrong indices, against the wrong slice. The seeds already imply it;
    /// both are written, because the seeds constraint is the one an implementer
    /// is most likely to simplify away.
    #[account(
        mut,
        close = owner,
        has_one = owner @ PerpsError::NotPositionOwner,
        has_one = market @ PerpsError::WrongMarket,
        seeds = [b"position", market.key().as_ref(), owner.key().as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(address = exchange.collateral_mint @ PerpsError::WrongCollateralMint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [b"quote_vault"],
        bump = pool.vault_bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_token_account.mint == exchange.collateral_mint @ PerpsError::WrongCollateralMint,
        constraint = owner_token_account.owner == owner.key() @ PerpsError::NotTokenOwner,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,
}

/// Accounts for [`crate::sakura_perps::admin_settle_position`].
///
/// [`ClosePosition`]'s, plus an admin signer, and with the owner demoted from a
/// signer to a constrained payee.
#[derive(Accounts)]
pub struct AdminSettlePosition<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(mut, seeds = [b"market", market.feed_id.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,

    #[account(address = market.price_update @ PerpsError::WrongPriceUpdate)]
    pub price_update: Box<Account<'info, PriceUpdateV2>>,

    /// CHECK: never read, only credited with the closed position's rent. Its
    /// identity is proven by `has_one = owner` on the position below, which
    /// asserts exactly what an `address = position.owner` here would. The
    /// assertion has to be made there rather than here: Anchor validates fields
    /// in declaration order, the position's seeds name this account, and the two
    /// constraints cannot both point backwards.
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,

    /// `has_one = market` is the constraint whose absence let a position opened
    /// in market A be settled against market B. Written alongside the seeds,
    /// which already imply it, because the seeds are what an implementer is most
    /// likely to simplify away.
    #[account(
        mut,
        close = owner,
        has_one = owner @ PerpsError::NotPositionOwner,
        has_one = market @ PerpsError::WrongMarket,
        seeds = [b"position", market.key().as_ref(), owner.key().as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(address = exchange.collateral_mint @ PerpsError::WrongCollateralMint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [b"quote_vault"],
        bump = pool.vault_bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Pinned to the **position's** owner, not to whoever the admin nominated.
    /// Without this constraint an admin names their own token account as the
    /// payout destination and a liquidation becomes a transfer to the
    /// liquidator, with the trader's rent as the only thing they get back.
    #[account(
        mut,
        constraint = owner_token_account.mint == exchange.collateral_mint @ PerpsError::WrongCollateralMint,
        constraint = owner_token_account.owner == position.owner @ PerpsError::NotTokenOwner,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,
}

/// Accounts for [`crate::sakura_perps::emergency_close_position`].
///
/// [`AdminSettlePosition`]'s, minus the `price_update`, plus the quarantine
/// constraint. **The absence of the price account is the instruction**: there is
/// nothing here an oracle outage, a revoked feed or a widened confidence band
/// can gate, because there is no oracle account to gate.
#[derive(Accounts)]
pub struct EmergencyClosePosition<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    /// Quarantined, as a constraint rather than a handler check. Quarantine is
    /// `max_oi_usd == 0`, so it is the same bit that stops new positions being
    /// opened — an admin cannot wind a market down without first having closed
    /// it to new risk, and the delay in the handler runs from the moment they
    /// did.
    #[account(
        mut,
        seeds = [b"market", market.feed_id.as_ref()],
        bump = market.bump,
        constraint = market.is_quarantined() @ PerpsError::MarketNotQuarantined,
    )]
    pub market: Box<Account<'info, Market>>,

    /// CHECK: as [`AdminSettlePosition::owner`].
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,

    #[account(
        mut,
        close = owner,
        has_one = owner @ PerpsError::NotPositionOwner,
        has_one = market @ PerpsError::WrongMarket,
        seeds = [b"position", market.key().as_ref(), owner.key().as_ref()],
        bump = position.bump,
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(address = exchange.collateral_mint @ PerpsError::WrongCollateralMint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [b"quote_vault"],
        bump = pool.vault_bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Pinned to the position's owner, for the reason
    /// [`AdminSettlePosition::owner_token_account`] gives.
    #[account(
        mut,
        constraint = owner_token_account.mint == exchange.collateral_mint @ PerpsError::WrongCollateralMint,
        constraint = owner_token_account.owner == position.owner @ PerpsError::NotTokenOwner,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,
}

/// Host-side tests for the two ledgers.
///
/// They run under `cargo test -p sakura-perps --lib`: no LiteSVM, no compiled
/// `.so`, no Solana toolchain. That is the point. The property being checked
/// here — that recorded liabilities never exceed the vault balance — is the one
/// whose violation makes a position **permanently unclosable**, in a milestone
/// that ships no keeper liquidation, and a test that needs a build toolchain to
/// run is a test that gets run once.
///
/// The account plumbing is not simulated. `apply_open_ledger` and
/// `apply_close_ledger` are the whole of the arithmetic that can break I1, which
/// is why they are functions rather than blocks inside their handlers.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::tests::{test_market, test_pool};
    use sakura_perps_risk::scale::PRICE_SCALE;

    /// USDC. `usd_to_quote_floor` is `mul_div_floor(usd, 10^d, USD_SCALE)` with
    /// `USD_SCALE == 1_000_000`, so at six decimals it is the **identity** and
    /// the conversions contribute no rounding surplus at all. Every ledger line
    /// therefore has to be exact rather than conservative-by-accident, and this
    /// is the decimal count that proves it.
    const DECIMALS: u8 = 6;

    /// Two units of a nine-decimal asset.
    const SIZE_BASE: u64 = 2_000_000_000;
    const ENTRY_PRICE: u128 = 100 * PRICE_SCALE;
    /// Thirty USDC, already net of the open fee — the field's own meaning.
    const COLLATERAL_QUOTE: u64 = 30_000_000;
    /// One thousand USDC of liquidity-provider equity.
    const LP_EQUITY: u64 = 1_000_000_000;

    /// Exactly the sum `assert_vault_solvent` compares against the vault
    /// balance. `reserved_quote` is deliberately absent: a reserve is a claim
    /// against liquidity-provider equity, not a liability on top of it.
    fn liabilities(pool: &Pool) -> u64 {
        pool.quote_deposited + pool.locked_quote + pool.pending_protocol_fees
    }

    struct Opened {
        position: Position,
        open_fee_quote: u64,
        collateral_deposited_quote: u64,
    }

    /// A position exactly as `open_position` step 5 to step 10 would have
    /// written it, so the ledger under test is fed the numbers it will really
    /// see rather than round ones chosen to make the arithmetic tidy.
    fn opened(side: u8, market: &Market) -> Opened {
        let entry_notional_usd =
            notional_usd_ceil(u128::from(SIZE_BASE), ENTRY_PRICE, market.asset_decimals).unwrap();
        let open_fee_quote = to_u64(
            usd_to_quote_ceil(
                trade_fee(entry_notional_usd, market.open_fee_bps).unwrap(),
                DECIMALS,
            )
            .unwrap(),
        )
        .unwrap();
        let reserve_quote = to_u64(
            usd_to_quote_ceil(
                profit_cap_usd(entry_notional_usd, market.max_profit_bps).unwrap(),
                DECIMALS,
            )
            .unwrap(),
        )
        .unwrap();

        Opened {
            position: Position {
                bump: 255,
                owner: Pubkey::default(),
                market: Pubkey::default(),
                side,
                size_base: SIZE_BASE,
                entry_price: ENTRY_PRICE,
                entry_notional_usd,
                collateral_quote: COLLATERAL_QUOTE,
                reserve_quote,
                maintenance_margin_bps: market.maintenance_margin_bps,
                liquidation_fee_bps: market.liquidation_fee_bps,
                close_fee_bps: market.close_fee_bps,
                entry_borrow_index: market.cum_borrow_index,
                entry_funding_index: market.cum_funding_index,
                opened_ts: 0,
                opened_slot: 0,
                spread_bps: market.spread_bps,
                _reserved: [0u8; 62],
            },
            open_fee_quote,
            // Step 7 read backwards: the gross the trader sent is what the
            // position holds plus the fee the vault kept.
            collateral_deposited_quote: COLLATERAL_QUOTE + open_fee_quote,
        }
    }

    struct RoundTrip {
        pool: Pool,
        market: Market,
        vault: u64,
        settled: LiquidatedSettlement,
        /// Signed. The admin path's liquidatability gate reads it, and a
        /// negative value is the bad-debt case B1 turns on.
        equity_usd: i128,
        /// The close fee **as computed**, before `settle_close` clamped it. The
        /// number B1 says must never be booked.
        close_fee_usd: u128,
        /// Fee revenue after the open leg, which legitimately booked the open
        /// fee. The close-leg assertions compare against these rather than
        /// against zero, or they would be asserting that the open fee vanished.
        pending_after_open: u64,
        deposited_after_open: u64,
    }

    /// Open a position and close it at `exit_price`, asserting I1 at every step.
    ///
    /// The ordinary and emergency paths are `liq_fee_usd == 0`; the admin path
    /// is everything else. All three share one ledger, which is what makes a
    /// single harness the right shape rather than a convenience.
    fn round_trip(side: u8, exit_price: u128, protocol_share_bps: u16) -> RoundTrip {
        round_trip_with_liq_fee(side, exit_price, protocol_share_bps, 0)
    }

    fn round_trip_with_liq_fee(
        side: u8,
        exit_price: u128,
        protocol_share_bps: u16,
        liq_fee_usd: u128,
    ) -> RoundTrip {
        let mut pool = test_pool(LP_EQUITY, 0);
        let mut market = test_market();
        let open = opened(side, &market);

        // The vault starts **exactly** solvent, with no surplus. That is the
        // case worth testing: there is no slack for a mis-booked fee to hide in.
        let mut vault = liabilities(&pool);

        vault += open.collateral_deposited_quote;
        let before = liabilities(&pool);
        apply_open_ledger(
            &mut pool,
            &mut market,
            &open.position,
            open.open_fee_quote,
            protocol_share_bps,
        )
        .unwrap();
        assert_eq!(
            liabilities(&pool) - before,
            open.collateral_deposited_quote,
            "liabilities must rise by exactly what the vault received"
        );
        assert_eq!(vault, liabilities(&pool), "I1, at the open leg");
        let pending_after_open = pool.pending_protocol_fees;
        let deposited_after_open = pool.quote_deposited;

        let valuation = value_close(&open.position, &market, DECIMALS, exit_price).unwrap();
        let close_fee_usd =
            trade_fee(valuation.exit_notional_usd, open.position.close_fee_bps).unwrap();
        let equity_usd = valuation.equity_usd;
        let settled = apply_liquidation_fee(valuation.settlement, liq_fee_usd, DECIMALS).unwrap();
        // The property the payout rests on, asserted before it is spent: the
        // four base-unit amounts re-sum, so `net_payout_quote` is representable
        // and no path can transfer more than the vault let go of.
        assert_eq!(
            settled.gross_payout_quote,
            settled.close_fee_quote + settled.liquidation_fee_quote + settled.net_payout_quote
        );

        let before = liabilities(&pool);
        apply_close_ledger(
            &mut pool,
            &mut market,
            &open.position,
            &settled,
            protocol_share_bps,
        )
        .unwrap();
        let net_payout_quote = to_u64(settled.net_payout_quote).unwrap();
        vault -= net_payout_quote;
        assert_eq!(
            before - liabilities(&pool),
            net_payout_quote,
            "liabilities must fall by exactly what left the vault"
        );
        assert_eq!(vault, liabilities(&pool), "I1, at the close leg");

        RoundTrip {
            pool,
            market,
            vault,
            settled,
            equity_usd,
            close_fee_usd,
            pending_after_open,
            deposited_after_open,
        }
    }

    /// **B1.** The single most important test in the milestone.
    ///
    /// A long closing at a price that wiped out its collateral. `settle_close`
    /// returns `close_fee_quote == 0` because there is no payout to take a fee
    /// from, and the ledger must book that zero rather than the fee it asked
    /// for. Booking the pre-clamp figure credits the pool money the vault never
    /// received; `quote_deposited + locked_quote + pending_protocol_fees` then
    /// exceeds `quote_vault.amount`, the closing solvency assertion reverts the
    /// transaction, and the position can never be closed again — by anyone, on
    /// any path, because M5 ships no keeper liquidation.
    #[test]
    fn a_close_at_non_positive_equity_books_no_fee_and_still_balances() {
        let rt = round_trip(SIDE_LONG, 50 * PRICE_SCALE, 3_000);

        // Non-vacuous: a fee really was computed. A test where the input was
        // zero would pass with the bug still present.
        assert!(
            rt.close_fee_usd > 0,
            "the close fee must have been computed"
        );
        assert_eq!(rt.settled.close_fee_quote, 0, "B1: the clamped fee is zero");
        assert_eq!(rt.settled.net_payout_quote, 0);
        assert_eq!(rt.settled.gross_payout_quote, 0);
        assert!(
            rt.settled.bad_debt_usd > 0,
            "the shortfall must be recorded"
        );
        assert_eq!(
            rt.market.cum_bad_debt_usd, rt.settled.bad_debt_usd,
            "bad debt is recorded on the market, never socialised"
        );

        // The close booked nothing as fee revenue, on either side of the split.
        // The open fee is what these are compared against: it was retained, so
        // booking it was correct, and asserting zero here would only prove the
        // test had forgotten about it.
        assert_eq!(rt.pool.pending_protocol_fees, rt.pending_after_open);
        assert_eq!(
            rt.pool.quote_deposited,
            rt.deposited_after_open + COLLATERAL_QUOTE,
            "liquidity providers keep the collateral and nothing more"
        );

        // And the failure the rule exists to prevent, spelled out. There is no
        // surplus in the vault, so booking the *pre-clamp* fee would push
        // recorded liabilities above the balance by exactly that fee.
        let unclamped = to_u64(usd_to_quote_ceil(rt.close_fee_usd, DECIMALS).unwrap()).unwrap();
        assert!(unclamped > 0);
        assert!(
            liabilities(&rt.pool) + unclamped > rt.vault,
            "booking the unclamped fee makes the vault insolvent on paper"
        );
    }

    /// The same, for a short — where a *rising* price is the ruinous one.
    #[test]
    fn a_short_at_non_positive_equity_books_no_fee_either() {
        let rt = round_trip(SIDE_SHORT, 200 * PRICE_SCALE, 3_000);

        assert!(rt.close_fee_usd > 0);
        assert_eq!(rt.settled.close_fee_quote, 0);
        assert_eq!(rt.pool.pending_protocol_fees, rt.pending_after_open);
        assert!(rt.settled.bad_debt_usd > 0);
    }

    /// I1 as a ledger property over the open and close arithmetic together.
    ///
    /// Both sides, both fee splits at their extremes, and exit prices spanning
    /// total loss, ordinary loss, breakeven, ordinary profit and a profit large
    /// enough that the cap binds. The assertions live inside `round_trip`, which
    /// checks the vault and the liabilities move by the same amount at each leg
    /// and end equal — an equality of differences, with no dust in either
    /// direction.
    #[test]
    fn the_ledger_balances_across_the_parameter_space() {
        for side in [SIDE_LONG, SIDE_SHORT] {
            for multiple in [50u128, 90, 99, 100, 101, 110, 200] {
                // Zero, the maximum the program permits, and something between.
                for protocol_share_bps in [0u16, 1, 1_500, crate::MAX_PROTOCOL_FEE_SHARE_BPS] {
                    let rt = round_trip(side, multiple * PRICE_SCALE, protocol_share_bps);
                    assert_eq!(
                        rt.vault,
                        liabilities(&rt.pool),
                        "side {side}, exit {multiple}x, share {protocol_share_bps}bps"
                    );
                }
            }
        }
    }

    /// Open interest is subtracted at **entry** notional, so the counters return
    /// to exactly zero however far the price moved in between.
    ///
    /// Subtracting exit notional instead leaves a residue proportional to the
    /// move, the open-interest cap drifts out of meaning, and I4 — a side has
    /// open interest if and only if it has positions — is what would eventually
    /// catch it, long after the cap stopped capping anything.
    #[test]
    fn a_round_trip_returns_every_counter_to_zero() {
        for side in [SIDE_LONG, SIDE_SHORT] {
            let exit_price = 137 * PRICE_SCALE;
            let market = test_market();
            let open = opened(side, &market);
            let exit_notional_usd =
                notional_usd_ceil(u128::from(SIZE_BASE), exit_price, market.asset_decimals)
                    .unwrap();
            // Non-vacuous: the price genuinely moved, so entry and exit notional
            // are different numbers and only one of them returns the counter.
            assert_ne!(exit_notional_usd, open.position.entry_notional_usd);

            let rt = round_trip(side, exit_price, 3_000);

            assert_eq!(rt.market.long_oi_usd, 0);
            assert_eq!(rt.market.short_oi_usd, 0);
            assert_eq!(rt.market.long_positions, 0);
            assert_eq!(rt.market.short_positions, 0);
            assert_eq!(rt.market.locked_quote, 0);
            assert_eq!(rt.market.reserved_quote, 0);
            assert_eq!(rt.pool.locked_quote, 0);
            assert_eq!(rt.pool.reserved_quote, 0);
        }
    }

    /// The pool never pays past the reserve it snapshotted at open.
    ///
    /// A long at twice its entry price is owed 200 USD of profit on 30 USD of
    /// collateral; what it gets is collateral plus `reserve_quote`, and the
    /// event says so through `profit_capped` rather than quietly paying less
    /// than the equity the trader can compute for themselves.
    #[test]
    fn a_profit_beyond_the_reserve_is_capped_at_it() {
        let market = test_market();
        let open = opened(SIDE_LONG, &market);
        let rt = round_trip(SIDE_LONG, 200 * PRICE_SCALE, 3_000);

        assert!(rt.settled.profit_capped);
        assert_eq!(
            rt.settled.gross_payout_quote,
            u128::from(COLLATERAL_QUOTE + open.position.reserve_quote)
        );
        assert_eq!(rt.settled.bad_debt_usd, 0);
        // The fee came out of the payout, not out of thin air.
        assert!(rt.settled.close_fee_quote > 0);
        assert_eq!(
            rt.settled.gross_payout_quote,
            rt.settled.close_fee_quote + rt.settled.net_payout_quote
        );
    }

    /// The fee split re-sums exactly, which is the line the I1 derivation rests
    /// on: liabilities rise by precisely the fee the vault kept, never by a
    /// rounded-apart pair that leaves an unattributed remainder in the vault.
    #[test]
    fn booking_a_fee_moves_liabilities_by_exactly_the_fee() {
        for share_bps in [0u16, 1, 1_500, crate::MAX_PROTOCOL_FEE_SHARE_BPS] {
            for fee_quote in [0u128, 1, 3, 7, 999, 1_000_000] {
                let mut pool = test_pool(LP_EQUITY, 0);
                let before = liabilities(&pool);
                book_fee(&mut pool, fee_quote, share_bps).unwrap();
                assert_eq!(
                    u128::from(liabilities(&pool) - before),
                    fee_quote,
                    "fee {fee_quote}, share {share_bps}bps"
                );
            }
        }
    }

    /// I1 on the **admin** path, where both fee splits run.
    ///
    /// The specification's ledger property names this path specifically,
    /// because it is the only one that books two fees and the second is clamped
    /// against what the first left. The liquidation fees swept here include ones
    /// far larger than the whole payout — which is not an exotic case but the
    /// ordinary late liquidation, where equity has decayed to a few dollars
    /// while the fee is still computed on full notional.
    #[test]
    fn an_admin_settlement_balances_with_both_fees_booked() {
        for side in [SIDE_LONG, SIDE_SHORT] {
            for multiple in [50u128, 90, 99, 100, 101, 110, 200] {
                for protocol_share_bps in [0u16, 1, 1_500, crate::MAX_PROTOCOL_FEE_SHARE_BPS] {
                    for liq_fee_usd in [0u128, 1, 1_000_000, 10_000_000_000] {
                        let rt = round_trip_with_liq_fee(
                            side,
                            multiple * PRICE_SCALE,
                            protocol_share_bps,
                            liq_fee_usd,
                        );
                        assert_eq!(
                            rt.vault,
                            liabilities(&rt.pool),
                            "side {side}, exit {multiple}x, share {protocol_share_bps}bps, \
                             liquidation fee {liq_fee_usd}"
                        );
                    }
                }
            }
        }
    }

    /// **B3.** The liquidation fee is clamped against what the close fee left of
    /// the payout, never against notional alone.
    ///
    /// Without that clamp the transfer underflows and the position becomes
    /// permanently unliquidatable — on the only liquidation path this milestone
    /// ships, since there is no keeper. The fee asked for here is orders of
    /// magnitude larger than the position; what the vault keeps is exactly what
    /// was there.
    #[test]
    fn a_liquidation_fee_larger_than_the_payout_takes_only_what_is_there() {
        let rt = round_trip_with_liq_fee(SIDE_LONG, 99 * PRICE_SCALE, 3_000, u128::from(u64::MAX));

        // Non-vacuous: the position really was worth something, and the close
        // fee really did come out first.
        assert!(rt.equity_usd > 0);
        assert!(rt.settled.gross_payout_quote > 0);
        assert!(rt.settled.close_fee_quote > 0);

        assert_eq!(
            rt.settled.liquidation_fee_quote,
            rt.settled.gross_payout_quote - rt.settled.close_fee_quote,
            "the fee takes everything the close fee left, and not a unit more"
        );
        assert_eq!(rt.settled.net_payout_quote, 0);
        assert_eq!(rt.vault, liabilities(&rt.pool), "I1 still holds");
    }

    /// The maintenance requirement is read off **current** notional.
    ///
    /// Entry notional is right for funding, borrow and open interest, which are
    /// charged on the exposure the trader contracted for. It is wrong for
    /// health: a short whose price has doubled carries twice the exposure, and
    /// judged at entry notional it would carry half its true requirement. This
    /// is the equity that makes the two answers differ.
    #[test]
    fn the_maintenance_requirement_is_read_off_current_notional() {
        let market = test_market();
        let open = opened(SIDE_SHORT, &market);
        let maintenance = open.position.maintenance_margin_bps;

        let entry_notional_usd = open.position.entry_notional_usd;
        let current_notional_usd = notional_usd_ceil(
            u128::from(SIZE_BASE),
            2 * ENTRY_PRICE,
            market.asset_decimals,
        )
        .unwrap();
        assert_eq!(current_notional_usd, 2 * entry_notional_usd);

        let entry_requirement = margin_requirement(entry_notional_usd, maintenance).unwrap();
        let equity_usd = i128::try_from(entry_requirement + 1).unwrap();

        assert!(
            !is_liquidatable(equity_usd, entry_notional_usd, maintenance).unwrap(),
            "at entry notional this position looks healthy"
        );
        assert!(
            is_liquidatable(equity_usd, current_notional_usd, maintenance).unwrap(),
            "at current notional it is not, and current notional is what is used"
        );
    }

    /// The emergency reference is the market's last good price, falling back to
    /// the position's own entry price when the market has never priced anything.
    #[test]
    fn the_emergency_reference_falls_back_to_the_entry_price() {
        let mut market = test_market();
        let open = opened(SIDE_LONG, &market);

        assert_eq!(market.last_good_price, 0, "a market that never priced");
        assert_eq!(
            emergency_reference_price(&market, &open.position),
            open.position.entry_price
        );

        market.last_good_price = 137 * PRICE_SCALE;
        assert_eq!(
            emergency_reference_price(&market, &open.position),
            137 * PRICE_SCALE
        );
    }

    /// The emergency exit price can always be struck.
    ///
    /// `execution_price` refuses a price outright once `confidence + spread >=
    /// mid`, and the whole point of `emergency_close_position` is that nothing
    /// can make it refuse. Confidence is zero there because no oracle was read,
    /// which leaves only the spread — and every legal spread is at most
    /// `MAX_SPREAD_BPS` of the mid, so the adverse adjustment is strictly less
    /// than the mid at every reference price down to one unit. An arithmetic
    /// argument that is not tested is a comment.
    #[test]
    fn the_emergency_exit_price_is_strikeable_at_every_legal_spread() {
        for side in [Side::Long, Side::Short] {
            for spread_bps in [0u16, 1, 10, 250, crate::MAX_SPREAD_BPS] {
                for reference in [1u128, 2, 9_999, PRICE_SCALE, 137 * PRICE_SCALE] {
                    let struck =
                        execution_price(side, PriceDirection::Close, reference, 0, spread_bps);
                    let price = struck.unwrap_or_else(|err| {
                        panic!("unstrikeable at {reference} with {spread_bps}bps: {err:?}")
                    });
                    assert!(price > 0, "a zero exit price makes the position unvaluable");
                }
            }
        }
    }
}
