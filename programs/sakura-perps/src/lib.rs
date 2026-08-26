//! Sakura Perps — permissionless oracle-and-pool perpetual futures.
//!
//! Traders trade against a shared liquidity pool at an oracle price, in the
//! shape Jupiter Perps and GMX use, rather than against a limit order book.
//! Market listing is permissionless but constrained: anyone may create a market
//! for an asset whose price feed the admin has already qualified, and a newly
//! created market opens quarantined with zero open-interest allowance until its
//! risk parameters are set. Permissionless *listing*, not permissionless
//! *risk-parameter setting* — the distinction is what keeps it safe.
//!
//! # Status
//!
//! Devnet only. Unaudited. Do not use with funds of real value.
//!
//! This file currently contains only `initialize_exchange` and the [`Exchange`]
//! config account. That is deliberate: the first milestone is a pipeline that
//! demonstrably builds, tests, and deploys, because the repository this replaced
//! never achieved any of the three. Markets, the liquidity pool, positions,
//! funding and liquidation land on top of a foundation that is known to work.
//!
//! # Conventions established here and relied on throughout
//!
//! * **Every cluster-varying address lives in an account, never a `const`.**
//!   The collateral mint, its token program, and the fee recipient are all
//!   fields of [`Exchange`]. A predecessor program hardcoded
//!   `pub const SAKURA_MINT`, which made it both untestable on devnet and
//!   permanently wrong on mainnet.
//! * **Token-2022 first.** All token types come from `anchor_spl::token_interface`.
//!   The collateral mint may be owned by either the legacy SPL Token program or
//!   Token-2022 — devnet USDC is legacy while SAKURA is Token-2022 — so the
//!   program pins whichever it was initialized with and rejects the other.
//! * **Checked arithmetic only.** No `saturating_*` in value-carrying math; it
//!   silently corrupts accounting at the boundary instead of failing loudly.
//! * **Space via `InitSpace`.** Never hand-counted byte arithmetic.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;
use sakura_perps_risk::oracle::OracleGuards;

pub mod market;
pub mod oracle;
pub mod pool;
pub mod position;

// Glob re-export, not a narrow import. `#[program]` generates references to
// `crate::__client_accounts_*` for every Accounts struct, so those macro-made
// modules have to be visible at the crate root even though the structs
// themselves live in `pool`, `market` or `position`.
pub use crate::market::*;
pub use crate::pool::*;
pub use crate::position::*;

declare_id!("5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y");

/// Basis-point denominator. 10_000 bps == 100%.
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Upper bound on the protocol's cut of trading fees, in basis points.
///
/// Capped in code rather than left to admin discretion so that "the admin can
/// set the protocol fee to 100% and starve liquidity providers" is not a finding
/// an auditor has to raise.
pub const MAX_PROTOCOL_FEE_SHARE_BPS: u16 = 3_000;

/// Ceiling on the keeper's cut of a liquidation fee.
///
/// Half. A keeper only has to be paid enough to cover the transaction and beat
/// doing nothing; beyond that the fee is being taken from the pool that
/// underwrote the position. The cap matters because this share is the one number
/// an admin could otherwise turn into a drain on every liquidation.
pub const MAX_KEEPER_FEE_SHARE_BPS: u16 = 5_000;

#[program]
pub mod sakura_perps {
    use super::*;

    /// Creates the singleton [`Exchange`] configuration account.
    ///
    /// Callable once. The `Exchange` PDA has fixed seeds, so a second call fails
    /// at account creation rather than needing an explicit guard.
    ///
    /// The collateral mint is captured here, along with the token program that
    /// owns it. Every later instruction that moves collateral asserts against
    /// the stored program id — `Interface<TokenInterface>` accepts both the
    /// legacy and Token-2022 programs, so without pinning, a caller could
    /// present the wrong one.
    pub fn initialize_exchange(
        ctx: Context<InitializeExchange>,
        params: InitializeExchangeParams,
    ) -> Result<()> {
        require!(
            params.protocol_fee_share_bps <= MAX_PROTOCOL_FEE_SHARE_BPS,
            PerpsError::ProtocolFeeShareTooHigh
        );
        require!(
            params.keeper_fee_share_bps <= MAX_KEEPER_FEE_SHARE_BPS,
            PerpsError::KeeperFeeShareTooHigh
        );

        let collateral_mint = &ctx.accounts.collateral_mint;

        // A freezable collateral mint means its freeze authority can brick both
        // withdrawals and liquidations at will. That is not a risk to accept on
        // behalf of liquidity providers by accident — but it is one this venue
        // has to accept on purpose, because every real USD stablecoin carries a
        // freeze authority and refusing all of them refuses USDC itself. So the
        // default stays closed and the admin opts in explicitly, once.
        let freeze_authority: Option<Pubkey> = collateral_mint.freeze_authority.into();
        require!(
            freeze_authority.is_none() || params.allow_freezable_collateral,
            PerpsError::CollateralMintIsFreezable
        );

        let exchange = &mut ctx.accounts.exchange;
        exchange.bump = ctx.bumps.exchange;
        exchange.admin = ctx.accounts.admin.key();
        // Admin transfer is two-step; there is no instruction that sets `admin`
        // directly. A single-step transfer to a mistyped address is unrecoverable.
        exchange.pending_admin = Pubkey::default();
        exchange.fee_recipient = params.fee_recipient;
        exchange.collateral_mint = collateral_mint.key();
        // `*mint.to_account_info().owner` is the token program that owns this
        // mint — the value later instructions must match against.
        exchange.collateral_token_program = *collateral_mint.to_account_info().owner;
        exchange.collateral_decimals = collateral_mint.decimals;
        // Recorded, not merely permitted: anyone auditing this exchange can see
        // which key can freeze its collateral without having to go and read the
        // mint. `default()` means none, matching the reserved-field convention.
        exchange.collateral_freeze_authority = freeze_authority.unwrap_or_default();
        exchange.protocol_fee_share_bps = params.protocol_fee_share_bps;
        exchange.keeper_fee_share_bps = params.keeper_fee_share_bps;
        // Everything starts paused. An exchange that is live the instant it is
        // created is an exchange nobody had a chance to inspect first.
        exchange.paused_flags = PauseFlags::ALL;
        exchange.num_markets = 0;

        emit!(ExchangeInitialized {
            exchange: exchange.key(),
            admin: exchange.admin,
            collateral_mint: exchange.collateral_mint,
            collateral_token_program: exchange.collateral_token_program,
            collateral_decimals: exchange.collateral_decimals,
        });

        Ok(())
    }

    /// Validate a price feed against a set of guards and emit the result.
    ///
    /// Changes no state. Its purpose is to answer, on chain and against the real
    /// account, the question that must be settled before a feed is qualified:
    /// *does this feed currently produce a price this protocol would trade on?*
    /// Guessing at that off-chain is how a market ends up listed against a feed
    /// with the wrong exponent, or one that stopped updating last Tuesday.
    ///
    /// Permissionless, because it is a read with no effect. It is also where an
    /// operator should start when a market has stopped trading and nobody knows
    /// which of the seven checks is failing — the returned error names it.
    ///
    /// # Why `feed_id` comes from instruction data here, and must not elsewhere
    ///
    /// A probe asks "does *this* account satisfy *these* guards for *this*
    /// feed", so all three necessarily come from the caller. Every instruction
    /// that moves money must instead take `feed_id` from the market's stored
    /// configuration. A caller-supplied id would simply be set to match whatever
    /// account was handed over, turning the SDK's feed-id check — the thing
    /// stopping a BONK price from reaching a SOL market — into a no-op.
    pub fn probe_oracle(ctx: Context<ProbeOracle>, params: ProbeOracleParams) -> Result<()> {
        let guards = OracleGuards {
            max_age_seconds: params.max_age_seconds,
            max_age_slots: params.max_age_slots,
            max_future_skew_seconds: params.max_future_skew_seconds,
            max_confidence_bps: params.max_confidence_bps,
            min_price: params.min_price,
            max_price: params.max_price,
            expected_exponent: params.expected_exponent,
        };

        let clock = Clock::get()?;
        let validated =
            oracle::load_price(&ctx.accounts.price_update, &params.feed_id, &guards, &clock)?;

        emit!(OracleProbed {
            price_update: ctx.accounts.price_update.key(),
            feed_id: params.feed_id,
            price: validated.price,
            confidence: validated.confidence,
            publish_time: validated.publish_time,
            posted_slot: validated.posted_slot,
            probed_at_slot: clock.slot,
        });

        Ok(())
    }

    /// Creates the shared liquidity pool, its collateral vault, and the LP share
    /// mint. Admin-only, and callable once — the PDA seeds make a second call
    /// fail at account creation rather than needing an explicit guard.
    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        params: InitializePoolParams,
    ) -> Result<()> {
        pool::handle_initialize_pool(ctx, params)
    }

    /// Deposits collateral and mints LP shares.
    ///
    /// `min_shares_out` is mandatory rather than advisory: without it a
    /// depositor has no defence against the share price moving between
    /// simulation and execution.
    pub fn lp_deposit(ctx: Context<LpDeposit>, amount: u64, min_shares_out: u64) -> Result<()> {
        pool::handle_lp_deposit(ctx, amount, min_shares_out)
    }

    /// Escrows shares and starts the withdrawal delay.
    pub fn request_withdraw(ctx: Context<RequestWithdraw>, shares: u64) -> Result<()> {
        pool::handle_request_withdraw(ctx, shares)
    }

    /// Burns escrowed shares and returns collateral, once the delay has elapsed.
    pub fn lp_withdraw(ctx: Context<LpWithdraw>, min_amount_out: u64) -> Result<()> {
        pool::handle_lp_withdraw(ctx, min_amount_out)
    }

    /// Closes an orphaned withdraw escrow, returning its rent to the owner.
    ///
    /// Recovery for accounts stranded by the version of `lp_withdraw` that
    /// closed the request but not its escrow, which left the owner unable to
    /// ever request a withdrawal again. Refuses to touch an escrow that still
    /// holds shares, or one whose request is still open.
    pub fn close_stale_escrow(ctx: Context<CloseStaleEscrow>) -> Result<()> {
        pool::handle_close_stale_escrow(ctx)
    }

    /// Abandons a pending withdrawal, returning the escrowed shares and closing
    /// both the request and its escrow.
    ///
    /// A request that cannot execute — the utilisation ceiling alone is enough
    /// to cause this — otherwise strands its shares forever: `lp_withdraw` keeps
    /// failing, and `close_stale_escrow` refuses a funded escrow by design.
    pub fn cancel_withdraw(ctx: Context<CancelWithdraw>) -> Result<()> {
        pool::handle_cancel_withdraw(ctx)
    }

    /// Sets the pause bitfield.
    ///
    /// The exchange is created with everything paused, which until now made it
    /// permanently inert — there was no instruction that could lift it. Writing
    /// the vault tests is what surfaced that: the first deposit any test tried
    /// failed with `DepositsPaused` and there was nothing to do about it.
    ///
    /// Admin-only. A later milestone should let a keeper *tighten* flags without
    /// being able to loosen them, so an automated circuit breaker can halt
    /// trading without also being able to restart it.
    pub fn set_pause_flags(ctx: Context<SetPauseFlags>, flags: u64) -> Result<()> {
        require!(flags <= PauseFlags::ALL, PerpsError::InvalidPauseFlags);

        let exchange = &mut ctx.accounts.exchange;
        let previous = exchange.paused_flags;
        exchange.paused_flags = flags;

        emit!(PauseFlagsChanged {
            exchange: exchange.key(),
            previous,
            current: flags,
        });

        Ok(())
    }

    /// Declares a Pyth feed safe to price markets against.
    ///
    /// Admin-only, and the single point at which oracle risk enters the
    /// protocol. Everything a market needs to know about its price source —
    /// the feed id, the exact `PriceUpdateV2` account, the exponent, the sanity
    /// band, both guard sets and the divergence tolerance — is fixed here and
    /// copied by value at `create_market`. That is what makes listing safe to
    /// leave permissionless: whoever creates a market picks a feed from this
    /// allowlist and nothing else.
    ///
    /// There is no re-qualification. The PDA seeds make a second call fail at
    /// account creation, and the alternative — editing a feed that markets have
    /// already copied — would let a position be settled under numbers it was
    /// never opened under.
    pub fn qualify_feed(ctx: Context<QualifyFeed>, params: QualifyFeedParams) -> Result<()> {
        market::handle_qualify_feed(ctx, params)
    }

    /// Flips a qualified feed's revocation bit.
    ///
    /// Revocation gates **opening only**: `create_market` and `open_position`
    /// read it, and closing, admin settlement, emergency close, price refresh
    /// and every liquidity-provider path do not. A feed the admin has stopped
    /// trusting is a reason to stop taking new risk on it and never a reason to
    /// trap the risk already there.
    ///
    /// It does not quarantine markets and does not close positions. One bit,
    /// reversible, touching no value.
    pub fn set_feed_revoked(ctx: Context<SetFeedRevoked>, revoked: bool) -> Result<()> {
        market::handle_set_feed_revoked(ctx, revoked)
    }

    /// Lists a market against a qualified feed. Permissionless.
    ///
    /// Any signer may pay the rent, because creating a market grants nothing:
    /// it is born quarantined with every risk parameter at zero, and
    /// `max_oi_usd == 0` means no position can be opened until an admin calls
    /// `set_risk_params`. Permissionless *listing*, not permissionless
    /// *risk-parameter setting*.
    pub fn create_market(ctx: Context<CreateMarket>) -> Result<()> {
        market::handle_create_market(ctx)
    }

    /// Sets a market's full risk-parameter block, activating or quarantining it.
    ///
    /// Admin-only and deliberately **not** pause-gated: an admin must be able to
    /// tighten a market while the protocol is paused, and quarantining it by
    /// setting `max_oi_usd = 0` is the tightest action available.
    ///
    /// There is no gate on open positions. Every parameter a position depends
    /// on is snapshotted at open or consumed at open, and the one change that
    /// could genuinely make a market unclosable — a raised borrow rate — is
    /// bounded by a ceiling rather than by a gate. A gate would have been worse
    /// than useless: raising the rate reads as tightening, and lowering it back
    /// would read as a loosening that the positions it bricked then block.
    pub fn set_risk_params(ctx: Context<SetRiskParams>, params: RiskParams) -> Result<()> {
        market::handle_set_risk_params(ctx, params)
    }

    /// Accrues a market's borrow and funding indices up to now. Permissionless.
    ///
    /// No signer and no pause gate, both deliberately. A pause that stops the
    /// clock is a subsidy to whoever is paying, and an authority-gated accrual
    /// would mean the protocol's clock ran only while somebody with a key was
    /// paying attention.
    ///
    /// It reads **no oracle**. Requiring a fresh price would make settlement
    /// fail exactly when the oracle is degraded, which is when accrual matters
    /// most. It does read the pool, because borrow accrues against pool-wide
    /// utilisation — so borrow is coupled across markets, and opening a position
    /// in one raises the borrow rate in every other.
    ///
    /// The same routine runs at the head of every position instruction. This one
    /// exists so accrual does not depend on trading activity.
    pub fn settle_market(ctx: Context<SettleMarket>) -> Result<()> {
        market::handle_settle_market(ctx)
    }

    /// Advances a market's oracle-free settlement reference. Permissionless.
    ///
    /// Loads under the trading guards, clamps the mid into its own EMA band and
    /// writes `last_good_price`. Touches no value, moves no tokens, and grants
    /// the caller nothing.
    ///
    /// It exists so that reference cannot be frozen. Without it, an admin could
    /// pause opening and closing, wait for the market to move, and then
    /// emergency-close every position at the stale price those two instructions
    /// had left behind. With it, anyone can advance the reference at any time
    /// for the cost of one transaction, and freezing it requires the feed itself
    /// to be dead — in which case `last_good_price` genuinely is the last honest
    /// price there was. That is why it is neither pausable nor admin-gated.
    pub fn refresh_market_price(ctx: Context<RefreshMarketPrice>) -> Result<()> {
        market::handle_refresh_market_price(ctx)
    }

    /// Sets the pool's deposit cap and its utilisation ceiling. Admin-only.
    ///
    /// `max_utilization_bps` had no setter at all until now — it was written
    /// once at `initialize_pool` and the live devnet pool was stuck with
    /// whatever it was given. That matters more than it sounds: the ceiling is
    /// this milestone's bound on how far an LP share price can be overstated,
    /// capped at [`M5_MAX_UTILIZATION_BPS`], so leaving it unsettable meant the
    /// bound could not actually be applied to a running pool.
    pub fn set_pool_limits(
        ctx: Context<SetPoolLimits>,
        max_aum_quote: u64,
        max_utilization_bps: u16,
    ) -> Result<()> {
        pool::handle_set_pool_limits(ctx, max_aum_quote, max_utilization_bps)
    }

    /// Opens an isolated position against the shared liquidity pool.
    ///
    /// One position per owner per market, created with `init` and never
    /// `init_if_needed`: the second would silently overwrite a live position's
    /// entry price and indices while the pool still held its collateral and its
    /// reserve. Adding to a position is a later milestone.
    ///
    /// Thirteen ordered steps, and the order is part of the specification. Two
    /// of them carry the safety argument for this leg. Divergence between the
    /// spot price and its own EMA is a **rejection** here — the only leg where
    /// refusing is a safe default, because there is no position yet to trap —
    /// and every parameter the position will later be judged by is snapshotted
    /// onto it now, so no admin retune can reach an open position.
    ///
    /// The collateral transfer is measured rather than assumed. A Token-2022
    /// mint carrying a transfer-fee extension delivers less than was sent, and
    /// booking a liability for the requested amount would break the solvency
    /// invariant on the spot.
    pub fn open_position(ctx: Context<OpenPosition>, params: OpenPositionParams) -> Result<()> {
        position::handle_open_position(ctx, params)
    }

    /// Closes a position the caller owns, settling it against the pool.
    ///
    /// Pause-gated and nothing more. **No quarantine check and no revocation
    /// check**: a market that has stopped accepting new risk must still let
    /// existing risk out, or every tightening action doubles as a trap.
    ///
    /// The exit price is clamped into the feed's own EMA band in **both**
    /// directions and never rejected. Rejecting at exit would be most valuable
    /// to a manipulator at exactly the moment it fired, and an adverse-only
    /// clamp would stop the pool paying out on a manipulated price while doing
    /// nothing to stop it charging on one.
    ///
    /// The spread applied is the position's snapshot, not the market's live
    /// value, so an admin cannot retroactively tax an exit — or, past
    /// `confidence + spread >= mid`, make one unpriceable.
    pub fn close_position(ctx: Context<ClosePosition>, params: ClosePositionParams) -> Result<()> {
        position::handle_close_position(ctx, params)
    }

    /// Liquidates a position that no longer meets its maintenance margin.
    ///
    /// The only forced exit this milestone ships — there is no permissionless
    /// keeper path — which is why every clamp on it is load-bearing: if this
    /// instruction cannot settle a position, nothing can.
    ///
    /// Priced under the **liquidation** guards, which may legitimately be the
    /// looser of the two: refusing to liquidate is not a safe default, because a
    /// position the pool cannot close is one it underwrites for free while the
    /// loss grows. Health is judged at **current** notional rather than entry
    /// notional, since the maintenance requirement is a statement about the
    /// exposure that exists now.
    ///
    /// The payout destination is pinned to the position's own owner. An admin
    /// naming their own token account would turn a liquidation into a transfer
    /// to the liquidator, and nothing else in the account list would notice.
    pub fn admin_settle_position(ctx: Context<AdminSettlePosition>) -> Result<()> {
        position::handle_admin_settle_position(ctx)
    }

    /// Liquidates an underwater position. **Anyone may call this.**
    ///
    /// Settles identically to `admin_settle_position` — same pause gate, same
    /// liquidation guards, same fee and clamps — with two differences: the signer
    /// is unconstrained, and it is paid `exchange.keeper_fee_share_bps` of the
    /// liquidation fee that was already being charged.
    ///
    /// The safety property is that a solvent position is untouchable: the gate is
    /// `is_liquidatable` at current notional, and a caller who does not meet it
    /// gets `PositionNotLiquidatable` regardless of who they are. The trader's
    /// payout is pinned to the position's own owner, so an arbitrary caller
    /// cannot redirect it — that constraint is load-bearing here in a way it is
    /// not on the admin path.
    ///
    /// Closes §9.4: with only the admin path, positions decayed past their
    /// collateral at whatever pace an admin ran, and bad debt accumulated
    /// unbounded.
    pub fn liquidate_position(ctx: Context<LiquidatePosition>) -> Result<()> {
        position::handle_liquidate_position(ctx)
    }

    /// Winds a position down with **no oracle at all**.
    ///
    /// This is the exit that has to survive everything else failing, so it takes
    /// no price account, no feed account and no pause gate, and it settles
    /// against `market.last_good_price` — a number kept on the market itself by
    /// every successful price read, including the permissionless
    /// `refresh_market_price`, precisely so that pausing the trading paths
    /// cannot freeze it.
    ///
    /// Loosening the oracle guards was the alternative and it is not a fix: a
    /// loosened guard still fails when the oracle is *absent*, and absent is
    /// what revocation, delisting and an outage all produce.
    ///
    /// Two preconditions, both public and both slow: the market must be
    /// quarantined, and it must have been for a day. A wind-down is therefore
    /// announced by the chain before any value moves.
    pub fn emergency_close_position(ctx: Context<EmergencyClosePosition>) -> Result<()> {
        position::handle_emergency_close_position(ctx)
    }
}

/// Accounts for [`sakura_perps::set_pause_flags`].
#[derive(Accounts)]
pub struct SetPauseFlags<'info> {
    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(mut, seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,
}

/// Emitted whenever the pause bitfield changes, so the transition is auditable
/// from logs rather than only inferable from account diffs.
#[event]
pub struct PauseFlagsChanged {
    pub exchange: Pubkey,
    pub previous: u64,
    pub current: u64,
}

/// Arguments to [`sakura_perps::probe_oracle`].
///
/// Every guard is explicit rather than defaulted. A probe whose thresholds were
/// implicit would answer a different question from the one the caller asked, and
/// the whole point is to establish exactly which threshold a feed fails.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ProbeOracleParams {
    /// The 32-byte Pyth feed id this account is expected to carry.
    pub feed_id: [u8; 32],
    /// Maximum age by upstream publish time, in seconds.
    pub max_age_seconds: u32,
    /// Maximum age by slots since the update landed on chain.
    pub max_age_slots: u64,
    /// Tolerance for a publish time ahead of the cluster clock, in seconds.
    pub max_future_skew_seconds: u32,
    /// Maximum confidence interval as a fraction of price, in basis points.
    pub max_confidence_bps: u16,
    /// Lower bound of the sanity band, at `PRICE_SCALE`.
    pub min_price: u128,
    /// Upper bound of the sanity band, at `PRICE_SCALE`.
    pub max_price: u128,
    /// The exponent the feed is expected to publish.
    pub expected_exponent: i32,
}

/// Accounts for [`sakura_perps::probe_oracle`].
#[derive(Accounts)]
pub struct ProbeOracle<'info> {
    /// The Pyth price update to inspect.
    ///
    /// `Account<PriceUpdateV2>` enforces that this is genuinely owned by the
    /// Pyth receiver program; an arbitrary account with convincing-looking bytes
    /// fails deserialisation rather than being believed.
    pub price_update: Box<Account<'info, PriceUpdateV2>>,
}

/// Emitted by [`sakura_perps::probe_oracle`] when a feed passes every check.
#[event]
pub struct OracleProbed {
    /// The price update account inspected.
    pub price_update: Pubkey,
    /// The feed id it was checked against.
    pub feed_id: [u8; 32],
    /// Validated price, at `PRICE_SCALE`.
    pub price: u128,
    /// Validated confidence interval, at `PRICE_SCALE`.
    pub confidence: u128,
    /// Upstream publish time.
    pub publish_time: i64,
    /// Slot at which the update landed on chain.
    pub posted_slot: u64,
    /// Slot at which the probe ran, so staleness is reconstructible from the log.
    pub probed_at_slot: u64,
}

/// Arguments to [`sakura_perps::initialize_exchange`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitializeExchangeParams {
    /// Destination for the protocol's share of trading fees.
    pub fee_recipient: Pubkey,
    /// Protocol's cut of trading fees in bps; the remainder accrues to LPs.
    pub protocol_fee_share_bps: u16,
    /// The keeper's cut of a **liquidation** fee in bps, capped at
    /// [`MAX_KEEPER_FEE_SHARE_BPS`]. A split of the existing fee, never an
    /// addition to it. Zero means liquidation stays permissionless but unpaid.
    pub keeper_fee_share_bps: u16,
    /// Accept a collateral mint that carries a freeze authority.
    ///
    /// Defaults closed and has to be set deliberately. Every real USD stablecoin
    /// has a freeze authority — mainnet USDC's is `7dGbd2QZ…` — so a blanket
    /// refusal means the exchange can never hold the collateral it was designed
    /// around. Refusing by default is still right: a freezable mint nobody
    /// examined is how a venue ends up with an issuer able to brick withdrawals,
    /// and the admin picks this mint exactly once, permanently.
    ///
    /// Setting it asserts the issuer's freeze power was weighed and accepted.
    /// The authority is recorded on the [`Exchange`] so that decision is
    /// auditable on-chain rather than living in a deploy script.
    pub allow_freezable_collateral: bool,
}

/// Bitfield of independently pausable actions.
///
/// A single global `paused: bool` forces an operator to choose between leaving
/// an exploit running and trapping every user's funds. Separate flags mean
/// opening can be halted while closing, withdrawing and liquidating continue —
/// which is almost always the correct response to an incident.
pub struct PauseFlags;

impl PauseFlags {
    pub const OPEN_POSITION: u64 = 1 << 0;
    pub const CLOSE_POSITION: u64 = 1 << 1;
    pub const LP_DEPOSIT: u64 = 1 << 2;
    pub const LP_WITHDRAW: u64 = 1 << 3;
    pub const LIQUIDATE: u64 = 1 << 4;
    pub const CREATE_MARKET: u64 = 1 << 5;
    pub const ALL: u64 = 0b11_1111;
}

/// Singleton exchange configuration. Seeds: `[b"exchange"]`.
#[account]
#[derive(InitSpace)]
pub struct Exchange {
    pub bump: u8,
    /// Current admin. Changed only via the two-step `pending_admin` handshake.
    pub admin: Pubkey,
    /// Proposed admin, who must call an accept instruction to take over.
    pub pending_admin: Pubkey,
    /// Where the protocol's fee share is sent.
    pub fee_recipient: Pubkey,
    /// Collateral and settlement mint for every market.
    pub collateral_mint: Pubkey,
    /// Token program owning `collateral_mint` — legacy SPL Token or Token-2022.
    /// Pinned at initialization; later instructions must match it exactly.
    pub collateral_token_program: Pubkey,
    /// Cached from the mint. Read at runtime, never assumed — a predecessor
    /// program assumed 9 decimals for a 6-decimal mint and was wrong by 1000x.
    pub collateral_decimals: u8,
    /// Freeze authority of `collateral_mint` at initialization, or the default
    /// pubkey when it had none. Stored so the one party able to brick this
    /// exchange's withdrawals is visible on-chain rather than implied.
    pub collateral_freeze_authority: Pubkey,
    /// Bitfield of [`PauseFlags`].
    pub paused_flags: u64,
    /// Protocol's share of trading fees in bps; the rest goes to LPs.
    pub protocol_fee_share_bps: u16,
    /// Number of markets created so far.
    pub num_markets: u32,
    /// The keeper's share of a **liquidation** fee, in bps; the remainder splits
    /// between protocol and LPs exactly as before.
    ///
    /// This is a split of the existing fee, never an addition to it. A trader
    /// being liquidated pays what `liquidation_fee_bps` always charged, so
    /// enabling keepers cannot reprice a position that is already open.
    ///
    /// **Zero is the meaningful default.** The live devnet exchange predates this
    /// field and its reserve bytes are zero, so it reads 0 and liquidation keeps
    /// paying the pool exactly as it does today — permissionless liquidation
    /// still works, keepers just earn nothing until an admin sets a share. An
    /// uninitialised read failing to "pay nobody" rather than "pay everything" is
    /// the direction that cannot lose money.
    pub keeper_fee_share_bps: u16,
    /// Anchor has no migration story and fields always get added. Reserve now,
    /// because growing an account later means reallocating every instance.
    ///
    /// 128 originally; `collateral_freeze_authority` and `keeper_fee_share_bps`
    /// were taken from here rather than appended, which is what the reserve is
    /// for — the account's size is unchanged and no existing instance needs
    /// reallocating.
    pub _reserved: [u8; 94],
}

// Frozen. There is a live devnet exchange, and `InitSpace` is the only place
// this number is written down; a silent change to it orphans that account.
const _: () = assert!(Exchange::INIT_SPACE == 304);

#[derive(Accounts)]
pub struct InitializeExchange<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + Exchange::INIT_SPACE,
        seeds = [b"exchange"],
        bump,
    )]
    pub exchange: Box<Account<'info, Exchange>>,

    /// Collateral mint. `InterfaceAccount` accepts a mint owned by either the
    /// legacy SPL Token program or Token-2022; which one it actually is gets
    /// recorded on the exchange and enforced from then on.
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    pub system_program: Program<'info, System>,
}

#[event]
pub struct ExchangeInitialized {
    pub exchange: Pubkey,
    pub admin: Pubkey,
    pub collateral_mint: Pubkey,
    pub collateral_token_program: Pubkey,
    pub collateral_decimals: u8,
}

#[error_code]
pub enum PerpsError {
    #[msg("Protocol fee share exceeds the maximum permitted by the program.")]
    ProtocolFeeShareTooHigh,
    #[msg(
        "Collateral mint has a freeze authority, which could brick withdrawals and liquidations."
    )]
    CollateralMintIsFreezable,
    #[msg("Arithmetic overflow.")]
    MathOverflow,

    // ── Oracle ──────────────────────────────────────────────────────────────
    // Each rejection keeps its own variant. An operator debugging a market that
    // has stopped trading needs to know whether the feed is stale, has widened,
    // or has drifted outside its band -- those have different responses.
    #[msg("Oracle price could not be read, or failed Pyth verification.")]
    OraclePriceUnavailable,
    #[msg("Oracle price is older than this market permits.")]
    OracleStale,
    #[msg("Oracle confidence interval is too wide to trade on.")]
    OracleConfidenceTooWide,
    #[msg("Oracle price is outside the market's configured sanity band.")]
    OraclePriceOutOfBand,
    #[msg("Oracle exponent differs from the value recorded when the feed was qualified.")]
    OracleExponentChanged,
    #[msg("Oracle price claims to have been published in the future.")]
    OraclePriceFromTheFuture,
    #[msg("Oracle price is zero or negative.")]
    OracleInvalidPrice,

    // ── Mapped from the risk crate ──────────────────────────────────────────
    #[msg("Amount must be non-negative.")]
    NegativeAmount,
    #[msg("Position size must be non-zero.")]
    ZeroSize,
    #[msg("Basis points exceed 10000.")]
    InvalidBasisPoints,
    #[msg("Pool has no shares outstanding.")]
    EmptyPool,
    #[msg("Initial margin must exceed maintenance margin plus liquidation fee.")]
    InvalidMarginParameters,
    #[msg("Trading guards must be no looser than liquidation guards.")]
    GuardsNotOrdered,

    // ── Pool and vault ──────────────────────────────────────────────────────
    #[msg("Only the exchange admin may do this.")]
    NotAdmin,
    #[msg("Deposit or withdraw fee exceeds the permitted maximum.")]
    FlowFeeTooHigh,
    #[msg("Withdrawal delay exceeds the permitted maximum.")]
    WithdrawDelayTooLong,
    #[msg("Liquidity deposits are paused.")]
    DepositsPaused,
    #[msg("Liquidity withdrawals are paused.")]
    WithdrawalsPaused,
    #[msg("Amount must be greater than zero.")]
    ZeroAmount,
    #[msg("Deposit would mint zero shares.")]
    ZeroSharesMinted,
    #[msg("Result was worse than the caller's stated minimum.")]
    SlippageExceeded,
    #[msg("Pool has reached its deposit cap.")]
    PoolCapReached,
    #[msg("Withdrawal exceeds liquidity providers' equity.")]
    InsufficientPoolEquity,
    #[msg("Withdrawal would push utilisation past the configured ceiling.")]
    UtilizationTooHigh,
    #[msg("Withdrawal delay has not elapsed.")]
    WithdrawTooSoon,
    #[msg("Withdraw request belongs to a different owner.")]
    NotRequestOwner,
    #[msg("Token account is not owned by the expected authority.")]
    NotTokenOwner,
    #[msg("Mint is not this exchange's collateral mint.")]
    WrongCollateralMint,
    #[msg("Mint is not this pool's share mint.")]
    WrongShareMint,
    #[msg("Token program does not match the one pinned at initialization.")]
    WrongTokenProgram,
    #[msg("Withdraw escrow still holds shares; complete the withdrawal instead.")]
    EscrowNotEmpty,
    #[msg("A withdraw request is still open; its escrow is not stale.")]
    WithdrawRequestStillOpen,
    #[msg("Vault balance is below the pool's recorded liabilities.")]
    VaultInsolvent,
    #[msg("Pause bitfield contains bits that are not defined.")]
    InvalidPauseFlags,

    // ── Markets and positions ───────────────────────────────────────────────
    // Appended, never inserted. Anchor numbers these positionally from 6000, so
    // a variant added in the middle renumbers every one after it and silently
    // relabels errors for the deployed IDL, the devnet clients and the SVM
    // tests -- all three of which compare the numeric code.
    #[msg("Feed parameters are outside the permitted ranges.")]
    InvalidFeedParameters,
    #[msg("Confidence gate plus the maximum spread would leave prices unstrikeable.")]
    ConfidenceGateTooWide,
    #[msg("This feed has been revoked; existing positions may still be closed.")]
    FeedRevoked,
    #[msg("Price update account is not the one this market was pinned to.")]
    WrongPriceUpdate,
    #[msg("Position belongs to a different market.")]
    WrongMarket,
    #[msg("Account is not the owner of this position.")]
    NotPositionOwner,
    #[msg("Market creation is paused.")]
    MarketCreationPaused,
    #[msg("Opening positions is paused.")]
    TradingPaused,
    #[msg("Closing positions is paused.")]
    ClosingPaused,
    #[msg("Liquidation is paused.")]
    LiquidationPaused,
    #[msg("Market is quarantined and will not accept new positions.")]
    MarketQuarantined,
    #[msg("Market is not quarantined, so emergency close is not available.")]
    MarketNotQuarantined,
    #[msg("Market has not been quarantined long enough to emergency-close.")]
    EmergencyCloseTooSoon,
    #[msg("Open interest on this side would exceed the market's cap.")]
    OpenInterestCapExceeded,
    #[msg("Profit cap is too large a multiple of the initial margin.")]
    ReserveLeverageTooHigh,
    #[msg("Borrow rate exceeds the program's ceiling.")]
    BorrowRateTooHigh,
    #[msg("Funding rate cap exceeds the program's ceiling.")]
    FundingRateTooHigh,
    #[msg("Funding sensitivity exceeds the program's ceiling.")]
    FundingSensitivityTooHigh,
    #[msg("Round-trip fees do not exceed the funding accruable over the policy holding period.")]
    FeesDoNotDominateFunding,
    #[msg("Round-trip fees do not cover the oracle drift a stale price permits.")]
    FeesDoNotDominateDrift,
    #[msg("Settlement window exceeds the permitted maximum.")]
    SettleWindowTooLong,
    #[msg("Risk parameters are outside the permitted ranges.")]
    InvalidRiskParameters,
    #[msg("Position is below one of the market's minimums.")]
    PositionTooSmall,
    #[msg("Collateral net of the open fee does not meet the initial margin requirement.")]
    InsufficientMargin,
    #[msg("Spot price diverges from the EMA by more than this market permits.")]
    PriceDiverged,
    #[msg("Position is not liquidatable at the current price.")]
    PositionNotLiquidatable,
    #[msg("Keeper fee share exceeds the maximum permitted by the program.")]
    KeeperFeeShareTooHigh,
    #[msg("Keeper token account must be owned by the keeper signing the liquidation.")]
    NotKeeperTokenOwner,
    #[msg("Utilisation ceiling is outside the range the program permits.")]
    UtilizationCeilingTooHigh,
    // Not in the specification's variant list, which never defines
    // `OpenPositionParams` at all. `side` is a `u8` on the wire and
    // `Position::is_long` tests it for equality with `SIDE_LONG`, so any third
    // value would book a short — a position facing the wrong way, with no error
    // for the caller to read. Rejecting it is cheaper than explaining it.
    #[msg("Position side must be either long or short.")]
    InvalidPositionSide,

    // The two below are invariant failures, expected to be unreachable in
    // correct operation. They are named anyway: without them a genuine
    // accounting drift surfaces as `MathOverflow` from the surrounding
    // `checked_*` idiom, which hides the one error worth waking up for.
    #[msg("Market's locked or reserved slice exceeds the pool's total.")]
    MarketSliceExceedsPool,
    #[msg("Market's position counters and open interest disagree.")]
    OpenInterestAccountingDrift,
}
