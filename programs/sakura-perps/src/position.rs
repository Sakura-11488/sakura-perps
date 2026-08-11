//! Open positions.
//!
//! Isolated margin, one position per owner per market, no adds. Seeds
//! `[b"position", market, owner]`, created with `init` — never
//! `init_if_needed`, which would silently overwrite a live position's
//! accounting with a new open.
//!
//! # Why so much is snapshotted
//!
//! `maintenance_margin_bps`, `liquidation_fee_bps` and `close_fee_bps` are
//! copied onto the position at open rather than read from the market at close.
//! Reading them live would mean an admin raising a market's maintenance margin
//! could make existing positions liquidatable in the same transaction — a
//! parameter change acting as a forced liquidation. Snapshotting means a
//! position is judged by the rules it was opened under.
//!
//! `reserve_quote` is snapshotted for a different reason: it is the single
//! authoritative number for what the pool has set aside for this position, so
//! the amount reserved at open and the cap applied at close cannot drift apart.

use anchor_lang::prelude::*;

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
    pub _reserved: [u8; 64],
}

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
