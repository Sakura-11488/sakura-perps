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

pub mod oracle;

declare_id!("5Va7HpaA9oRu9cqGXwvqwW3koqE1fBwsGcooFpL6jr2y");

/// Basis-point denominator. 10_000 bps == 100%.
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Upper bound on the protocol's cut of trading fees, in basis points.
///
/// Capped in code rather than left to admin discretion so that "the admin can
/// set the protocol fee to 100% and starve liquidity providers" is not a finding
/// an auditor has to raise.
pub const MAX_PROTOCOL_FEE_SHARE_BPS: u16 = 3_000;

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

        let collateral_mint = &ctx.accounts.collateral_mint;

        // A freezable collateral mint means its freeze authority can brick both
        // withdrawals and liquidations at will. That is not a risk to accept on
        // behalf of liquidity providers.
        require!(
            collateral_mint.freeze_authority.is_none(),
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
        exchange.protocol_fee_share_bps = params.protocol_fee_share_bps;
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
    /// Bitfield of [`PauseFlags`].
    pub paused_flags: u64,
    /// Protocol's share of trading fees in bps; the rest goes to LPs.
    pub protocol_fee_share_bps: u16,
    /// Number of markets created so far.
    pub num_markets: u32,
    /// Anchor has no migration story and fields always get added. Reserve now,
    /// because growing an account later means reallocating every instance.
    pub _reserved: [u8; 128],
}

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
}
