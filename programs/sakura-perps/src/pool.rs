//! The shared liquidity pool: custody, share accounting, deposit and withdraw.
//!
//! This is the first module that holds other people's money, so the whole design
//! is organised around one inequality that must never be false:
//!
//! ```text
//!   quote_vault.amount  >=  pool.quote_deposited      (liquidity providers' equity)
//!                         + pool.locked_quote         (trader collateral held on their behalf)
//!                         + pool.pending_protocol_fees (owed to the fee recipient)
//! ```
//!
//! It is asserted at the end of every instruction that touches the vault, by
//! reloading the token account and comparing. `>=` rather than `==`, so that
//! somebody transferring tokens directly into the vault is harmless rather than
//! a panic — see the next section for why that matters more than it sounds.
//!
//! It is the first of four, and [`assert_pool_invariants`] is the single place
//! they are all checked. The other three bound the reserve against tracked
//! equity, bound a market's slice by the pool's total, and tie a market's
//! position counters to its open interest. Every vault-touching instruction —
//! in this module and in the position instructions built on it — ends with that
//! one call, after `emit!` and after reloading the vault.
//!
//! # AUM is a tracked number, never the vault balance
//!
//! The single most important line in this module is the one that is *absent*:
//! nothing reads `quote_vault.amount` to price a share. Assets under management
//! come from `pool.quote_deposited`, which only changes when this program
//! decides it should.
//!
//! Deriving AUM from the balance is the ERC-4626 inflation attack, and it works
//! on Solana exactly as it does on Ethereum. An attacker deposits one base unit
//! for one share, transfers a large amount straight into the vault, and the next
//! depositor computes `floor(deposit × 1 / huge) = 0` shares — receiving nothing
//! while their money inflates the attacker's single share. Tracking equity
//! separately defeats it outright, because the donation never enters the
//! calculation.
//!
//! Two further defences sit on top, because one is never enough for a share
//! price: [`sakura_perps_risk::pool::MINIMUM_LIQUIDITY`] is minted to the pool
//! itself on the first deposit and can never be redeemed, and every deposit
//! carries a mandatory `min_shares_out`.
//!
//! # Withdrawal is two-step, deliberately
//!
//! A request is recorded, and only after `withdraw_delay_seconds` can it be
//! executed. This is not ceremony. Pool AUM jumps when a liquidation settles,
//! and a same-transaction deposit-then-withdraw brackets that jump for free.
//! Solana has no public mempool, but Jito bundles make bracketing an observed
//! transaction entirely practical, so the delay is what makes the sandwich
//! unprofitable rather than merely inconvenient.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
use sakura_perps_risk::math::{cmp_products, to_u64};
use sakura_perps_risk::pool as risk_pool;

use crate::{Exchange, Market, PauseFlags, PerpsError};

/// Decimals for the LP share mint.
///
/// Fixed at six regardless of the collateral mint's own decimals. The share is
/// a claim on the pool, not a wrapper around the collateral, and pinning it
/// means share maths never has to reason about two different scales at once.
pub const SHARE_DECIMALS: u8 = 6;

/// Upper bound on deposit and withdraw fees, in basis points.
///
/// These fees exist to price the sandwich described in the module docs, not to
/// extract from liquidity providers. Capped in code so "the admin can set the
/// withdrawal fee to 100% and trap every depositor" is not an available move.
pub const MAX_FLOW_FEE_BPS: u16 = 200;

/// Longest withdrawal delay an admin may configure.
///
/// A delay is a safety mechanism; an unbounded one is a freeze. Capped so the
/// admin cannot lock the pool indefinitely without it being an obvious
/// parameter change.
pub const MAX_WITHDRAW_DELAY_SECONDS: u32 = 24 * 60 * 60;

/// Hard ceiling on `pool.max_utilization_bps`: 20%.
///
/// This constant is milestone 5's entire answer to the LP share-pricing
/// question, and it is a **bound rather than a fix**. Share price comes off
/// `pool.quote_deposited`, which does not mark open positions to market, so it
/// can overstate what a share is worth. The overstatement is bounded by what is
/// reserved against those positions, and `reserved_quote / quote_deposited` is
/// exactly what this caps. So the worst-case mispricing, as a fraction of AUM,
/// **is** `max_utilization_bps`.
///
/// The alternative — computing the pool's true liability — was designed and
/// then deleted for cause. That liability is `Σ max(0, min(equity_i, cap_i))`,
/// and `max` and `min` do not commute with summation, so no aggregate over
/// summed size and summed entry notional can produce it. A milestone can have
/// an unsound estimate or no estimate; this one takes no estimate and records
/// the choice so it is not silently revisited.
pub const M5_MAX_UTILIZATION_BPS: u16 = 2_000;

/// The shared liquidity pool.
#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub bump: u8,
    /// Bump for the vault PDA, stored so signing does not re-derive it.
    pub vault_bump: u8,
    /// Mint of the LP share token. Authority is the pool PDA; freeze authority
    /// is deliberately `None` — a freezable share token lets an admin block
    /// redemptions, which is a trust red flag before it is anything else.
    pub share_mint: Pubkey,
    /// Program-owned token account holding collateral.
    pub quote_vault: Pubkey,
    /// Shares outstanding. Mirrors `share_mint.supply` and is asserted equal.
    pub total_shares: u64,
    /// Liquidity providers' equity. **This is AUM.** Never `quote_vault.amount`.
    pub quote_deposited: u64,
    /// Trader collateral held by the pool on their behalf. Not LP equity, and
    /// not available for withdrawal.
    pub locked_quote: u64,
    /// Accrued protocol fees, owed to `exchange.fee_recipient`. Not LP equity.
    pub pending_protocol_fees: u64,
    /// Reserved against open positions' potential profit.
    pub reserved_quote: u64,
    /// Fee charged on deposit, in bps.
    pub deposit_fee_bps: u16,
    /// Fee charged on withdrawal, in bps.
    pub withdraw_fee_bps: u16,
    /// Seconds between requesting a withdrawal and being able to execute it.
    pub withdraw_delay_seconds: u32,
    /// Utilisation ceiling. Bounded by [`M5_MAX_UTILIZATION_BPS`] at both the
    /// instruction that writes it and the one that created the pool.
    pub max_utilization_bps: u16,
    /// Hard cap on `quote_deposited`. Essential on the way to mainnet: it bounds
    /// what can be lost while the protocol is still unproven.
    pub max_aum_quote: u64,
    /// 128 originally, then 120 while a `min_liquidity_quote: u64` sat here.
    ///
    /// Those eight bytes were returned to the reserve rather than left
    /// declared-and-unused. The field was never written and never read: the real
    /// floor is [`sakura_perps_risk::pool::MINIMUM_LIQUIDITY`], a crate constant
    /// consumed inside `shares_for_deposit`. A field whose name promises a
    /// configurable minimum that does not exist is worse than no field at all —
    /// an operator sets it, observes nothing, and concludes the floor is off.
    /// `INIT_SPACE` is unchanged, and the live devnet pool already reads zero in
    /// those bytes, so nothing changes on chain.
    pub _reserved: [u8; 128],
}

/// A pending withdrawal. Created by `request_withdraw`, consumed by `lp_withdraw`.
///
/// Shares are moved into escrow on request rather than merely recorded, so the
/// requester cannot transfer them away and still redeem.
#[account]
#[derive(InitSpace)]
pub struct WithdrawRequest {
    pub bump: u8,
    pub owner: Pubkey,
    /// Shares held in escrow for this request.
    pub shares: u64,
    /// When the request was made. Execution is gated on this plus the delay.
    pub requested_at: i64,
    /// Slot of the request, so a same-slot request-and-execute is detectable
    /// even if the clock has not advanced a whole second.
    pub requested_slot: u64,
    pub _reserved: [u8; 64],
}

// Frozen: there is a live devnet pool, and Anchor has no migration story. Stage
// 3 adds no field to either of these, and this is what keeps that true.
const _: () = assert!(Pool::INIT_SPACE == 252);
const _: () = assert!(WithdrawRequest::INIT_SPACE == 121);

/// Arguments to [`crate::sakura_perps::initialize_pool`].
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitializePoolParams {
    pub deposit_fee_bps: u16,
    pub withdraw_fee_bps: u16,
    pub withdraw_delay_seconds: u32,
    pub max_utilization_bps: u16,
    pub max_aum_quote: u64,
}

pub fn handle_initialize_pool(
    ctx: Context<InitializePool>,
    params: InitializePoolParams,
) -> Result<()> {
    require!(
        params.deposit_fee_bps <= MAX_FLOW_FEE_BPS && params.withdraw_fee_bps <= MAX_FLOW_FEE_BPS,
        PerpsError::FlowFeeTooHigh
    );
    require!(
        params.withdraw_delay_seconds <= MAX_WITHDRAW_DELAY_SECONDS,
        PerpsError::WithdrawDelayTooLong
    );
    // The same predicate `set_pool_limits` enforces, and for the same reason:
    // `max_utilization_bps` **is** this milestone's bound on how far an LP share
    // price can be overstated, so a ceiling settable here but not there would
    // mean the bound held only for pools whose admin happened to call the
    // setter. A pool created at 9 999 bps admits reserving essentially every
    // asset it has, which is not a ceiling — and no later instruction forces a
    // correction, because nothing lowers the field on its own.
    require!(
        params.max_utilization_bps > 0 && params.max_utilization_bps <= M5_MAX_UTILIZATION_BPS,
        PerpsError::UtilizationCeilingTooHigh
    );

    let pool = &mut ctx.accounts.pool;
    pool.bump = ctx.bumps.pool;
    pool.vault_bump = ctx.bumps.quote_vault;
    pool.share_mint = ctx.accounts.share_mint.key();
    pool.quote_vault = ctx.accounts.quote_vault.key();
    pool.total_shares = 0;
    pool.quote_deposited = 0;
    pool.locked_quote = 0;
    pool.pending_protocol_fees = 0;
    pool.reserved_quote = 0;
    pool.deposit_fee_bps = params.deposit_fee_bps;
    pool.withdraw_fee_bps = params.withdraw_fee_bps;
    pool.withdraw_delay_seconds = params.withdraw_delay_seconds;
    pool.max_utilization_bps = params.max_utilization_bps;
    pool.max_aum_quote = params.max_aum_quote;

    emit!(PoolInitialized {
        pool: pool.key(),
        share_mint: pool.share_mint,
        quote_vault: pool.quote_vault,
        max_aum_quote: pool.max_aum_quote,
    });

    Ok(())
}

pub fn handle_lp_deposit(ctx: Context<LpDeposit>, amount: u64, min_shares_out: u64) -> Result<()> {
    let exchange = &ctx.accounts.exchange;
    require!(
        exchange.paused_flags & PauseFlags::LP_DEPOSIT == 0,
        PerpsError::DepositsPaused
    );
    require!(amount > 0, PerpsError::ZeroAmount);

    // Measure what actually arrived rather than trusting the requested amount.
    // A Token-2022 mint with a transfer fee delivers less than was sent, and
    // crediting the sent amount would hand the depositor shares the pool never
    // received backing for. The collateral mint is validated at exchange
    // initialization to have no such extension, so this is belt and braces —
    // but the belt costs ~300 compute units and the braces have failed before.
    let before = ctx.accounts.quote_vault.amount;

    token_interface::transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.depositor_token_account.to_account_info(),
                mint: ctx.accounts.collateral_mint.to_account_info(),
                to: ctx.accounts.quote_vault.to_account_info(),
                authority: ctx.accounts.depositor.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.collateral_mint.decimals,
    )?;

    ctx.accounts.quote_vault.reload()?;
    let received = ctx
        .accounts
        .quote_vault
        .amount
        .checked_sub(before)
        .ok_or(PerpsError::MathOverflow)?;
    require!(received > 0, PerpsError::ZeroAmount);

    let pool = &mut ctx.accounts.pool;
    // Snapshotted before anything moves, for I2's monotone form. A deposit
    // raises `quote_deposited` and never touches `reserved_quote`, so it can
    // only lower utilisation — and it must not be blocked by a ceiling an admin
    // lowered, because it is one of only two actions that bring the pool back
    // under one.
    let utilisation_before = UtilisationCheck::NotWorsened {
        reserved_quote: pool.reserved_quote,
        quote_deposited: pool.quote_deposited,
    };

    // Fee comes off the top, and rounds up: the pool never under-collects.
    let fee = to_u64(
        risk_pool::flow_fee(received as u128, pool.deposit_fee_bps)
            .map_err(crate::oracle::map_risk_error)?,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let net = received.checked_sub(fee).ok_or(PerpsError::MathOverflow)?;
    require!(net > 0, PerpsError::ZeroAmount);

    require!(
        pool.quote_deposited
            .checked_add(net)
            .ok_or(PerpsError::MathOverflow)?
            <= pool.max_aum_quote,
        PerpsError::PoolCapReached
    );

    // AUM from tracked equity, never from the vault balance. See module docs.
    let minted = risk_pool::shares_for_deposit(
        net as u128,
        pool.total_shares as u128,
        pool.quote_deposited as u128,
    )
    .map_err(crate::oracle::map_risk_error)?;

    let to_depositor = to_u64(minted.to_depositor).map_err(crate::oracle::map_risk_error)?;
    let total_minted = to_u64(minted.total().map_err(crate::oracle::map_risk_error)?)
        .map_err(crate::oracle::map_risk_error)?;

    // Slippage protection is mandatory, not optional. Without it a depositor
    // has no defence against the share price moving between simulation and
    // execution.
    require!(to_depositor >= min_shares_out, PerpsError::SlippageExceeded);
    require!(to_depositor > 0, PerpsError::ZeroSharesMinted);

    let pool_seeds: &[&[u8]] = &[b"pool", &[pool.bump]];
    let signer: &[&[&[u8]]] = &[pool_seeds];

    token_interface::mint_to(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            token_interface::MintTo {
                mint: ctx.accounts.share_mint.to_account_info(),
                to: ctx.accounts.depositor_share_account.to_account_info(),
                authority: pool.to_account_info(),
            },
            signer,
        ),
        to_depositor,
    )?;

    // The locked minimum is minted to the pool's own share account, where
    // nothing can redeem it. Non-zero only on the very first deposit.
    let locked = to_u64(minted.locked_forever).map_err(crate::oracle::map_risk_error)?;
    if locked > 0 {
        token_interface::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                token_interface::MintTo {
                    mint: ctx.accounts.share_mint.to_account_info(),
                    to: ctx.accounts.pool_share_account.to_account_info(),
                    authority: pool.to_account_info(),
                },
                signer,
            ),
            locked,
        )?;
    }

    pool.quote_deposited = pool
        .quote_deposited
        .checked_add(net)
        .ok_or(PerpsError::MathOverflow)?;
    pool.pending_protocol_fees = pool
        .pending_protocol_fees
        .checked_add(fee)
        .ok_or(PerpsError::MathOverflow)?;
    pool.total_shares = pool
        .total_shares
        .checked_add(total_minted)
        .ok_or(PerpsError::MathOverflow)?;

    emit!(LiquidityDeposited {
        pool: pool.key(),
        depositor: ctx.accounts.depositor.key(),
        amount_in: received,
        fee,
        shares_minted: to_depositor,
        total_shares: pool.total_shares,
        quote_deposited: pool.quote_deposited,
    });

    assert_pool_invariants(&ctx.accounts.quote_vault, pool, None, utilisation_before)
}

pub fn handle_request_withdraw(ctx: Context<RequestWithdraw>, shares: u64) -> Result<()> {
    require!(shares > 0, PerpsError::ZeroAmount);

    // Shares move into escrow rather than being merely noted, so they cannot be
    // transferred away and still redeemed.
    token_interface::transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.owner_share_account.to_account_info(),
                mint: ctx.accounts.share_mint.to_account_info(),
                to: ctx.accounts.escrow_share_account.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        shares,
        SHARE_DECIMALS,
    )?;

    let clock = Clock::get()?;
    let request = &mut ctx.accounts.withdraw_request;
    request.bump = ctx.bumps.withdraw_request;
    request.owner = ctx.accounts.owner.key();
    request.shares = shares;
    request.requested_at = clock.unix_timestamp;
    request.requested_slot = clock.slot;

    emit!(WithdrawRequested {
        pool: ctx.accounts.pool.key(),
        owner: request.owner,
        shares,
        executable_at: clock
            .unix_timestamp
            .checked_add(ctx.accounts.pool.withdraw_delay_seconds as i64)
            .ok_or(PerpsError::MathOverflow)?,
    });

    Ok(())
}

pub fn handle_lp_withdraw(ctx: Context<LpWithdraw>, min_amount_out: u64) -> Result<()> {
    let exchange = &ctx.accounts.exchange;
    require!(
        exchange.paused_flags & PauseFlags::LP_WITHDRAW == 0,
        PerpsError::WithdrawalsPaused
    );

    let clock = Clock::get()?;
    let request = &ctx.accounts.withdraw_request;

    // Both clocks. The timestamp is the policy; the slot check additionally
    // forbids request-and-execute inside one slot even where the delay is
    // configured as zero, which closes the atomic sandwich outright.
    require!(
        request.requested_slot < clock.slot,
        PerpsError::WithdrawTooSoon
    );
    let executable_at = request
        .requested_at
        .checked_add(ctx.accounts.pool.withdraw_delay_seconds as i64)
        .ok_or(PerpsError::MathOverflow)?;
    require!(
        clock.unix_timestamp >= executable_at,
        PerpsError::WithdrawTooSoon
    );

    let shares = request.shares;
    let pool = &mut ctx.accounts.pool;

    let gross = to_u64(
        risk_pool::assets_for_shares(
            shares as u128,
            pool.total_shares as u128,
            pool.quote_deposited as u128,
        )
        .map_err(crate::oracle::map_risk_error)?,
    )
    .map_err(crate::oracle::map_risk_error)?;

    // `assets_for_shares` returns 0 rather than erroring when the shares round
    // down to nothing, and every check below passes trivially on zero: the fee
    // is 0, `net >= min_amount_out` holds for anyone who sent 0, and
    // `gross <= quote_deposited` is vacuous. The burn and the `total_shares`
    // decrement would still execute, destroying the position for no payout and
    // handing the value to the remaining providers. Mirrors the `net > 0` guard
    // `lp_deposit` already carries.
    require!(gross > 0, PerpsError::ZeroAmount);

    let fee = to_u64(
        risk_pool::flow_fee(gross as u128, pool.withdraw_fee_bps)
            .map_err(crate::oracle::map_risk_error)?,
    )
    .map_err(crate::oracle::map_risk_error)?;
    let net = gross.checked_sub(fee).ok_or(PerpsError::MathOverflow)?;
    require!(net > 0, PerpsError::ZeroAmount);

    require!(net >= min_amount_out, PerpsError::SlippageExceeded);

    // Never pay out more than liquidity providers actually own. Reaching into
    // trader collateral or protocol fees would be theft dressed as a withdrawal.
    require!(
        gross <= pool.quote_deposited,
        PerpsError::InsufficientPoolEquity
    );

    let quote_deposited_after = pool
        .quote_deposited
        .checked_sub(gross)
        .ok_or(PerpsError::MathOverflow)?;

    // Withdrawing the liquidity reserved against open positions' profit is how
    // a departing provider front-runs traders who are owed money. Both GMX and
    // Jupiter enforce an equivalent ceiling.
    require!(
        risk_pool::withdrawal_leaves_enough_reserve(
            quote_deposited_after as u128,
            pool.reserved_quote as u128,
            pool.max_utilization_bps,
        )
        .map_err(crate::oracle::map_risk_error)?,
        PerpsError::UtilizationTooHigh
    );

    let pool_seeds: &[&[u8]] = &[b"pool", &[pool.bump]];
    let signer: &[&[&[u8]]] = &[pool_seeds];

    // Burn from escrow, not from the owner: the shares have already left them.
    token_interface::burn(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            token_interface::Burn {
                mint: ctx.accounts.share_mint.to_account_info(),
                from: ctx.accounts.escrow_share_account.to_account_info(),
                authority: pool.to_account_info(),
            },
            signer,
        ),
        shares,
    )?;

    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.quote_vault.to_account_info(),
                mint: ctx.accounts.collateral_mint.to_account_info(),
                to: ctx.accounts.owner_token_account.to_account_info(),
                authority: pool.to_account_info(),
            },
            signer,
        ),
        net,
        ctx.accounts.collateral_mint.decimals,
    )?;

    // Close the escrow token account, not just the request.
    //
    // `withdraw_request` carries `close = owner`, but Anchor's `close` cannot
    // touch a token account — SPL owns it, so it takes a CPI. Without this the
    // escrow PDA at `[b"withdraw_escrow", owner]` survives the withdrawal, and
    // the owner's NEXT request_withdraw fails at account creation ("already in
    // use") with no instruction anywhere able to clear it: a liquidity provider
    // could withdraw exactly once, ever, and the remainder of their position
    // would be stranded. Found by running the devnet round-trip a second time —
    // the SVM suite only ever exercised one withdrawal cycle.
    //
    // Safe here because the burn above left the escrow empty; `close_account`
    // refuses an account still holding tokens.
    token_interface::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        token_interface::CloseAccount {
            account: ctx.accounts.escrow_share_account.to_account_info(),
            destination: ctx.accounts.owner.to_account_info(),
            authority: pool.to_account_info(),
        },
        signer,
    ))?;

    // The fee stays in the vault and becomes protocol revenue; only `net` left.
    pool.quote_deposited = quote_deposited_after;
    pool.pending_protocol_fees = pool
        .pending_protocol_fees
        .checked_add(fee)
        .ok_or(PerpsError::MathOverflow)?;
    pool.total_shares = pool
        .total_shares
        .checked_sub(shares)
        .ok_or(PerpsError::MathOverflow)?;

    emit!(LiquidityWithdrawn {
        pool: pool.key(),
        owner: ctx.accounts.owner.key(),
        shares,
        amount_out: net,
        fee,
        total_shares: pool.total_shares,
        quote_deposited: pool.quote_deposited,
    });

    ctx.accounts.quote_vault.reload()?;
    // The ceiling, absolutely. A withdrawal removes the very equity the reserve
    // is measured against, so it is one of the two paths that can raise
    // utilisation and one of the two that must be gated on the value in force.
    assert_pool_invariants(
        &ctx.accounts.quote_vault,
        pool,
        None,
        UtilisationCheck::Ceiling,
    )
}

/// What the pool has recorded that it owes: liquidity-provider equity, trader
/// collateral held on their behalf, and fees owed to the recipient.
///
/// `reserved_quote` is deliberately absent. A reserve is a *claim against*
/// liquidity-provider equity, not a liability on top of it, so including it
/// would double-count and make a solvent pool look insolvent.
pub(crate) fn liabilities(pool: &Pool) -> Result<u64> {
    pool.quote_deposited
        .checked_add(pool.locked_quote)
        .and_then(|value| value.checked_add(pool.pending_protocol_fees))
        .ok_or(PerpsError::MathOverflow.into())
}

/// I1, over a balance rather than an account, so a host test can reach it.
///
/// A comment claiming an invariant holds is a hope. This is the check.
pub(crate) fn assert_solvent(vault_amount: u64, pool: &Pool) -> Result<()> {
    require!(
        vault_amount >= liabilities(pool)?,
        PerpsError::VaultInsolvent
    );
    Ok(())
}

fn assert_vault_solvent(vault: &InterfaceAccount<'_, TokenAccount>, pool: &Pool) -> Result<()> {
    assert_solvent(vault.amount, pool)
}

/// Which form of I2 an instruction is entitled to be judged by.
///
/// The distinction is not a convenience, it is a correctness requirement, and
/// getting it wrong bricks the protocol. `max_utilization_bps` may legitimately
/// be lowered below current utilisation — §3.9.1 permits it, and the setter caps
/// at [`M5_MAX_UTILIZATION_BPS`], so a pool whose ceiling was previously higher
/// cannot be raised back out of that state. Asserting the ceiling **absolutely**
/// at the end of every value-touching instruction then reverts *every close*,
/// including `emergency_close_position`, and reverts `lp_deposit` too — which is
/// the only other action that lowers utilisation. Every position and all
/// liquidity-provider equity is trapped, permanently, with no admin action and
/// no permissionless action that recovers it.
///
/// That is the exact failure mode the module docs warn about for I4 — "an
/// unconditionally-asserted falsified invariant bricks every instruction that
/// asserts it, including `close_position`" — reintroduced through I2.
///
/// So the ceiling binds where utilisation can **rise**, and everywhere else the
/// weaker monotone form applies: an instruction may not make utilisation worse
/// than it found it. The two coincide whenever the pool is already inside its
/// ceiling, which is every state reachable without an admin lowering it.
pub(crate) enum UtilisationCheck {
    /// The post-state must sit inside `pool.max_utilization_bps`.
    ///
    /// For `open_position`, which adds reserve, and `lp_withdraw`, which removes
    /// the equity the reserve is measured against. These are the two paths that
    /// take on risk, and they are gated absolutely.
    Ceiling,
    /// The post-state must be no worse than this pre-state.
    ///
    /// For the three settlement paths and `lp_deposit`, none of which can raise
    /// utilisation. A close lowers `reserved_quote` by the position's reserve
    /// `r` and lowers `quote_deposited` by at most `r` — `settle_close` caps the
    /// payout at collateral plus reserve — so with `reserved <= quote_deposited`
    /// the ratio cannot rise. Asserting that, rather than the ceiling, is what
    /// makes "lowering the ceiling invalidates no open position" true rather
    /// than merely intended.
    NotWorsened {
        reserved_quote: u64,
        quote_deposited: u64,
    },
}

/// I2, in whichever of its two forms the caller is entitled to.
fn assert_utilisation(pool: &Pool, check: &UtilisationCheck) -> Result<()> {
    match check {
        // The exact rational, not a floored utilisation: comparing floored
        // ratios admits an overhang of up to a basis point of AUM, and the harm
        // is not the dust — it is that the last position to close cannot be
        // paid.
        UtilisationCheck::Ceiling => require!(
            risk_pool::utilization_within_cap(
                u128::from(pool.reserved_quote),
                u128::from(pool.quote_deposited),
                pool.max_utilization_bps,
            )
            .map_err(crate::oracle::map_risk_error)?,
            PerpsError::UtilizationTooHigh
        ),
        // `reserved_after / deposited_after <= reserved_before /
        // deposited_before`, cross-multiplied so there is no division and no
        // rounding — the same discipline `utilization_within_cap` applies to the
        // ceiling. Through `cmp_products`, which multiplies into 256 bits, so
        // the comparison is exact without anyone having to check that a product
        // of two `u64`s fits a `u128`.
        UtilisationCheck::NotWorsened {
            reserved_quote,
            quote_deposited,
        } => require!(
            cmp_products(
                u128::from(pool.reserved_quote),
                u128::from(*quote_deposited),
                u128::from(*reserved_quote),
                u128::from(pool.quote_deposited),
            ) != core::cmp::Ordering::Greater,
            PerpsError::UtilizationTooHigh
        ),
    }
    Ok(())
}

/// I3 and I4, the two that need a market.
fn assert_market_invariants(pool: &Pool, market: &Market) -> Result<()> {
    // I3.
    require!(
        market.locked_quote <= pool.locked_quote && market.reserved_quote <= pool.reserved_quote,
        PerpsError::MarketSliceExceedsPool
    );

    // I4. Open interest is added at entry notional and subtracted at entry
    // notional, so both counters return to zero together or the accounting has
    // drifted. Unreachable in correct operation, which is precisely why it is
    // asserted rather than assumed — and why it has a construction test that
    // trips it deliberately, since it would otherwise never execute.
    require!(
        (market.long_positions == 0) == (market.long_oi_usd == 0)
            && (market.short_positions == 0) == (market.short_oi_usd == 0),
        PerpsError::OpenInterestAccountingDrift
    );

    Ok(())
}

/// Every invariant the caller is in a position to assert, in one call.
///
/// Four of them, and the split between the two arguments is the specification
/// rather than an implementation detail:
///
/// * **I1, solvency** — `assert_vault_solvent` above. Pool-wide.
/// * **I2, the reserve is honourable** — `reserved_quote / quote_deposited`
///   against the pool's ceiling, or against the pre-state, per
///   [`UtilisationCheck`]. Pool-wide. Read that type's docs before changing
///   which form a call site passes: one of the two choices bricks the protocol.
/// * **I3, the market slices sum** — pool-wide totals bound the touched
///   market's slice. Needs a market.
/// * **I4, the counters and open interest agree** — a side has open interest if
///   and only if it has positions. Needs a market.
///
/// `market` is `None` for the liquidity-provider paths, which move collateral
/// and protocol fees but touch no market slice: there is no market whose I3 and
/// I4 a deposit could have broken, and asserting them against an arbitrary one
/// would be theatre. Position instructions touch exactly one market and pass it.
///
/// # I2's denominator must never become a price
///
/// It is `quote_deposited` — tracked equity — and not an oracle-derived figure.
/// An assertion whose truth depends on a price can be falsified by the market
/// moving while nobody has done anything wrong, and an unconditionally-asserted
/// invariant that has been falsified **bricks every instruction that asserts
/// it, `close_position` included**. A previous design put a mark-to-market
/// quantity in an invariant and had exactly that effect.
///
/// I3 is the per-market bound rather than a cross-market sum: summing every
/// market is O(markets) and not assertable on chain, while the per-market bound
/// is O(1) and catches the error that actually matters — a market releasing
/// more than it reserved.
pub(crate) fn assert_pool_invariants(
    vault: &InterfaceAccount<'_, TokenAccount>,
    pool: &Pool,
    market: Option<&Market>,
    utilisation: UtilisationCheck,
) -> Result<()> {
    assert_vault_solvent(vault, pool)?;
    assert_utilisation(pool, &utilisation)?;

    let Some(market) = market else {
        return Ok(());
    };
    assert_market_invariants(pool, market)
}

#[derive(Accounts)]
pub struct InitializePool<'info> {
    #[account(mut, address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(
        init,
        payer = admin,
        space = 8 + Pool::INIT_SPACE,
        seeds = [b"pool"],
        bump,
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(address = exchange.collateral_mint @ PerpsError::WrongCollateralMint)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    /// Program-owned, PDA-seeded, deliberately not an associated token account.
    /// ATA derivation depends on the token program, and Token-2022 derives
    /// differently — explicit seeds leave no ambiguity about which account this
    /// is meant to be.
    #[account(
        init,
        payer = admin,
        seeds = [b"quote_vault"],
        bump,
        token::mint = collateral_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// LP share mint. Freeze authority is `None` by omission — a freezable
    /// share token would let an admin block redemptions.
    #[account(
        init,
        payer = admin,
        seeds = [b"share_mint"],
        bump,
        mint::decimals = SHARE_DECIMALS,
        mint::authority = pool,
        mint::token_program = token_program,
    )]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    /// Holds the permanently locked minimum liquidity.
    #[account(
        init,
        payer = admin,
        seeds = [b"pool_shares"],
        bump,
        token::mint = share_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub pool_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Pinned to whichever program owns the collateral mint. `Interface` accepts
    /// both the legacy and Token-2022 programs, so without this a caller could
    /// present the wrong one.
    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct LpDeposit<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

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

    #[account(mut, address = pool.share_mint @ PerpsError::WrongShareMint)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        constraint = depositor_token_account.mint == exchange.collateral_mint @ PerpsError::WrongCollateralMint,
        constraint = depositor_token_account.owner == depositor.key() @ PerpsError::NotTokenOwner,
    )]
    pub depositor_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = depositor_share_account.mint == pool.share_mint @ PerpsError::WrongShareMint,
        constraint = depositor_share_account.owner == depositor.key() @ PerpsError::NotTokenOwner,
    )]
    pub depositor_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, seeds = [b"pool_shares"], bump)]
    pub pool_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct RequestWithdraw<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(mut, address = pool.share_mint @ PerpsError::WrongShareMint)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        constraint = owner_share_account.mint == pool.share_mint @ PerpsError::WrongShareMint,
        constraint = owner_share_account.owner == owner.key() @ PerpsError::NotTokenOwner,
    )]
    pub owner_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// One request per owner. A second concurrent request fails at account
    /// creation rather than silently overwriting the first and stranding its
    /// escrowed shares.
    #[account(
        init,
        payer = owner,
        space = 8 + WithdrawRequest::INIT_SPACE,
        seeds = [b"withdraw_request", owner.key().as_ref()],
        bump,
    )]
    pub withdraw_request: Box<Account<'info, WithdrawRequest>>,

    #[account(
        init,
        payer = owner,
        seeds = [b"withdraw_escrow", owner.key().as_ref()],
        bump,
        token::mint = share_mint,
        token::authority = pool,
        token::token_program = token_program,
    )]
    pub escrow_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct LpWithdraw<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

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

    #[account(mut, address = pool.share_mint @ PerpsError::WrongShareMint)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    /// Closed on execution, returning rent to the owner. `has_one` binds the
    /// request to its signer, so one provider cannot execute another's.
    #[account(
        mut,
        close = owner,
        has_one = owner @ PerpsError::NotRequestOwner,
        seeds = [b"withdraw_request", owner.key().as_ref()],
        bump = withdraw_request.bump,
    )]
    pub withdraw_request: Box<Account<'info, WithdrawRequest>>,

    #[account(
        mut,
        seeds = [b"withdraw_escrow", owner.key().as_ref()],
        bump,
    )]
    pub escrow_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_token_account.mint == exchange.collateral_mint @ PerpsError::WrongCollateralMint,
        constraint = owner_token_account.owner == owner.key() @ PerpsError::NotTokenOwner,
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(address = exchange.collateral_token_program @ PerpsError::WrongTokenProgram)]
    pub token_program: Interface<'info, TokenInterface>,
}

#[event]
pub struct PoolInitialized {
    pub pool: Pubkey,
    pub share_mint: Pubkey,
    pub quote_vault: Pubkey,
    pub max_aum_quote: u64,
}

#[event]
pub struct LiquidityDeposited {
    pub pool: Pubkey,
    pub depositor: Pubkey,
    pub amount_in: u64,
    pub fee: u64,
    pub shares_minted: u64,
    pub total_shares: u64,
    pub quote_deposited: u64,
}

#[event]
pub struct WithdrawRequested {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub shares: u64,
    pub executable_at: i64,
}

#[event]
pub struct LiquidityWithdrawn {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub shares: u64,
    pub amount_out: u64,
    pub fee: u64,
    pub total_shares: u64,
    pub quote_deposited: u64,
}

/// Close an orphaned withdraw escrow, returning its rent to the owner.
///
/// # Why this exists
///
/// `request_withdraw` creates two accounts: a [`WithdrawRequest`] and an escrow
/// token account at `[b"withdraw_escrow", owner]`. An earlier `lp_withdraw`
/// closed only the first. The escrow survived every completed withdrawal, and
/// because `request_withdraw` *creates* that escrow, the owner's next request
/// failed at account creation — permanently, with nothing in the program able to
/// clear it. A provider got exactly one withdrawal and the remainder of their
/// position was stranded.
///
/// `lp_withdraw` closes the escrow now, so no new orphans are produced. This is
/// the migration path for the ones already on chain: at the time of writing a
/// live devnet account is in exactly this state and cannot withdraw again.
///
/// # Why it is safe to expose to anyone
///
/// It is owner-signed and refuses to do anything interesting:
///
/// - The escrow is addressed by PDA seeds, so an owner can only ever reach their
///   own. There is no account to pass that points at somebody else's.
/// - It requires the escrow to be **empty**. A non-empty escrow means shares are
///   still held against a live request, and discarding it would burn a
///   provider's claim; that case belongs to `lp_withdraw`, not here.
/// - It requires **no** [`WithdrawRequest`] to exist. With one open, the escrow
///   is load-bearing, and closing it would make that request permanently
///   unexecutable — which is the very failure this instruction exists to undo.
///
/// Rent goes back to the owner, who paid it. Deliberately not gated on
/// [`PauseFlags`]: a recovery path that a pause can disable is not a recovery
/// path, and closing an empty account moves no value.
pub fn handle_close_stale_escrow(ctx: Context<CloseStaleEscrow>) -> Result<()> {
    let escrow = &ctx.accounts.escrow_share_account;

    require!(escrow.amount == 0, PerpsError::EscrowNotEmpty);
    require!(
        ctx.accounts.withdraw_request.data_is_empty()
            && ctx.accounts.withdraw_request.lamports() == 0,
        PerpsError::WithdrawRequestStillOpen
    );

    let pool = &ctx.accounts.pool;
    let pool_seeds: &[&[u8]] = &[b"pool", &[pool.bump]];
    let signer: &[&[&[u8]]] = &[pool_seeds];

    // The pool is the escrow's token authority, so only the program can close
    // it — which is precisely why the orphan could not be cleared from outside.
    token_interface::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        token_interface::CloseAccount {
            account: escrow.to_account_info(),
            destination: ctx.accounts.owner.to_account_info(),
            authority: pool.to_account_info(),
        },
        signer,
    ))?;

    emit!(StaleEscrowClosed {
        pool: pool.key(),
        owner: ctx.accounts.owner.key(),
    });

    Ok(())
}

#[derive(Accounts)]
pub struct CloseStaleEscrow<'info> {
    /// Receives the reclaimed rent, and is the only signer able to reach this
    /// escrow — the seeds below bind the account to them.
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    /// Checked as raw account info because the whole point is that it must NOT
    /// exist. A typed `Account` would fail deserialization before the handler
    /// could give a meaningful error.
    /// CHECK: verified empty and unfunded in the handler; address is pinned by
    /// the same seeds `request_withdraw` uses, so it cannot point elsewhere.
    #[account(seeds = [b"withdraw_request", owner.key().as_ref()], bump)]
    pub withdraw_request: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"withdraw_escrow", owner.key().as_ref()],
        bump,
    )]
    pub escrow_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// Unconstrained here, exactly as in `request_withdraw` and `lp_withdraw`:
    /// the escrow is a typed `InterfaceAccount`, so Anchor already checks which
    /// token program owns it, and a mismatched program fails the CPI anyway.
    pub token_program: Interface<'info, TokenInterface>,
}

#[event]
pub struct StaleEscrowClosed {
    pub pool: Pubkey,
    pub owner: Pubkey,
}

/// Abandon a pending withdrawal and take the escrowed shares back.
///
/// Without this a request that cannot complete is a trap: the utilisation
/// ceiling alone is enough to make `lp_withdraw` fail indefinitely, and the
/// shares sit in escrow with no way out. `close_stale_escrow` deliberately
/// refuses that state — it requires an empty escrow and no live request, which
/// is the exact opposite of this case — so the two instructions are genuinely
/// distinct rather than one being a special case of the other.
///
/// Not gated on `PauseFlags`, for the same reason as `close_stale_escrow`:
/// withdrawing shares from escrow returns the owner to where they already were,
/// moves no pool assets, and a pause must not be able to strand them.
pub fn handle_cancel_withdraw(ctx: Context<CancelWithdraw>) -> Result<()> {
    let shares = ctx.accounts.withdraw_request.shares;
    let escrow = &ctx.accounts.escrow_share_account;

    // Pay back exactly what is in escrow, not what the request claims. If they
    // ever disagreed, the token account is the truth and the difference must
    // not be minted out of the discrepancy.
    require!(escrow.amount == shares, PerpsError::EscrowNotEmpty);
    require!(shares > 0, PerpsError::ZeroAmount);

    let pool = &ctx.accounts.pool;
    let pool_seeds: &[&[u8]] = &[b"pool", &[pool.bump]];
    let signer: &[&[&[u8]]] = &[pool_seeds];

    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: escrow.to_account_info(),
                mint: ctx.accounts.share_mint.to_account_info(),
                to: ctx.accounts.owner_share_account.to_account_info(),
                authority: pool.to_account_info(),
            },
            signer,
        ),
        shares,
        SHARE_DECIMALS,
    )?;

    // Close the escrow as well as the request. Leaving it behind is exactly the
    // orphan that made a provider's second `request_withdraw` fail with
    // "already in use" and required `close_stale_escrow` to exist.
    token_interface::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        token_interface::CloseAccount {
            account: escrow.to_account_info(),
            destination: ctx.accounts.owner.to_account_info(),
            authority: pool.to_account_info(),
        },
        signer,
    ))?;

    emit!(WithdrawCancelled {
        pool: pool.key(),
        owner: ctx.accounts.owner.key(),
        shares,
    });

    Ok(())
}

pub fn handle_set_pool_limits(
    ctx: Context<SetPoolLimits>,
    max_aum_quote: u64,
    max_utilization_bps: u16,
) -> Result<()> {
    // Strictly inside `(0, M5_MAX_UTILIZATION_BPS]`. The upper bound is the
    // milestone's entire answer to LP share mispricing — see the constant. The
    // strict lower bound is separate: a zero ceiling makes every open
    // impossible, which is what quarantining a market is for, and doing it
    // pool-wide by parameter would be an outage disguised as a setting.
    require!(
        max_utilization_bps > 0 && max_utilization_bps <= M5_MAX_UTILIZATION_BPS,
        PerpsError::UtilizationCeilingTooHigh
    );

    let pool = &mut ctx.accounts.pool;
    pool.max_aum_quote = max_aum_quote;
    pool.max_utilization_bps = max_utilization_bps;

    // Lowering the ceiling below current utilisation is **permitted**, and there
    // is deliberately no check for it. The temptation is to gate this on the
    // open book, and that is the mistake the struck retune rule was written for
    // — a gate that reads a tightening as safe and then refuses to let you
    // undo it.
    //
    // What a lowering blocks is exactly two things: `open_position` and
    // `lp_withdraw`, the two paths that can raise utilisation. It does **not**
    // block a close, an admin settlement, an emergency close, or a deposit —
    // those are judged by `UtilisationCheck::NotWorsened` instead, and that is
    // not a nicety. Judging them by the ceiling would mean an admin lowering it
    // below current utilisation reverted every exit *and* the deposits that
    // would bring utilisation back down, with the setter's own cap at
    // `M5_MAX_UTILIZATION_BPS` making the state unrecoverable. The claim that a
    // lowering invalidates no open position is true because of that split, not
    // in spite of it.
    //
    // Deliberately not followed by `assert_pool_invariants`: this instruction
    // moves no tokens, and asserting the ceiling here would turn the permitted
    // lowering into a revert.
    emit!(PoolLimitsChanged {
        pool: pool.key(),
        max_aum_quote: pool.max_aum_quote,
        max_utilization_bps: pool.max_utilization_bps,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct SetPoolLimits<'info> {
    #[account(seeds = [b"exchange"], bump = exchange.bump)]
    pub exchange: Box<Account<'info, Exchange>>,

    #[account(address = exchange.admin @ PerpsError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(mut, seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
}

#[derive(Accounts)]
pub struct CancelWithdraw<'info> {
    /// Receives both the escrowed shares and the reclaimed rent. The seeds below
    /// bind the request and escrow to this signer, so nobody can cancel anyone
    /// else's withdrawal.
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(seeds = [b"pool"], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,

    #[account(address = pool.share_mint @ PerpsError::WrongShareMint)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        constraint = owner_share_account.mint == pool.share_mint @ PerpsError::WrongShareMint,
        constraint = owner_share_account.owner == owner.key() @ PerpsError::NotTokenOwner,
    )]
    pub owner_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        close = owner,
        seeds = [b"withdraw_request", owner.key().as_ref()],
        bump = withdraw_request.bump,
    )]
    pub withdraw_request: Box<Account<'info, WithdrawRequest>>,

    #[account(
        mut,
        seeds = [b"withdraw_escrow", owner.key().as_ref()],
        bump,
    )]
    pub escrow_share_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Interface<'info, TokenInterface>,
}

#[event]
pub struct WithdrawCancelled {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub shares: u64,
}

/// The pool's two limits changed.
///
/// `max_utilization_bps` may be lowered below current utilisation. That blocks
/// new opens and new withdrawals until utilisation falls and invalidates no
/// open position, because the invariant is asserted per instruction against the
/// value in force at that moment. Gating the setter on open positions is the
/// tempting mistake and it is the one that made a market unclosable before.
#[event]
pub struct PoolLimitsChanged {
    pub pool: Pubkey,
    pub max_aum_quote: u64,
    pub max_utilization_bps: u16,
}

/// Host-side tests for the four invariants.
///
/// Two of them — I3 and I4 — are unreachable in correct operation, so nothing
/// else in the tree ever executes their `require!` bodies in either direction. A
/// typo that inverted one would compile, pass every other test, and ship as an
/// assertion that is permanently true or permanently false; a permanently false
/// one bricks every close. These are the construction tests that trip them
/// deliberately.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::tests::{test_market, test_pool};

    fn expect_error(result: Result<()>, expected: PerpsError) {
        let code = expected as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
        match result {
            Err(anchor_lang::error::Error::AnchorError(err)) => {
                assert_eq!(
                    err.error_code_number, code,
                    "expected {expected:?} ({code})"
                )
            }
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    }

    /// **I1.** The comparison is `>=`, so a donated surplus is harmless and a
    /// single missing base unit is not.
    #[test]
    fn solvency_is_the_liability_sum_and_nothing_else() {
        let mut pool = test_pool(1_000, 400);
        pool.locked_quote = 300;
        pool.pending_protocol_fees = 50;
        // `reserved_quote` is deliberately not in the sum: it is a claim against
        // liquidity-provider equity, not a liability on top of it. At 400 it
        // would push the requirement to 1 750 and fail this.
        assert_eq!(liabilities(&pool).unwrap(), 1_350);

        assert_solvent(1_350, &pool).expect("exactly solvent is solvent");
        assert_solvent(9_999, &pool).expect("a donated surplus is harmless");
        expect_error(assert_solvent(1_349, &pool), PerpsError::VaultInsolvent);
    }

    /// **I2, the blocker.** A settlement that strictly *improves* utilisation
    /// must not revert merely because an admin lowered the ceiling underneath
    /// it.
    ///
    /// The state is reachable, and it is the one stage 3 exists to enable: a
    /// pool initialised at a higher ceiling, positions opened legally under it,
    /// and then `set_pool_limits` called to bring the ceiling down to
    /// `M5_MAX_UTILIZATION_BPS`. Judged by the ceiling, every close reverts —
    /// including `emergency_close_position`, the designated escape hatch — and
    /// `lp_deposit`, the only other action that lowers utilisation, reverts too.
    /// The ceiling cannot be raised back, because the setter caps at exactly the
    /// value now being violated. Every position and all liquidity is trapped,
    /// permanently.
    #[test]
    fn a_close_that_improves_utilisation_survives_a_lowered_ceiling() {
        // 3 000 bps of utilisation under a ceiling since lowered to 2 000.
        let before = test_pool(1_000_000_000, 300_000_000);
        assert_eq!(before.max_utilization_bps, M5_MAX_UTILIZATION_BPS);

        // After a close: the reserve released, and liquidity providers credited
        // the trader's loss. Utilisation falls to roughly 2 786 bps — better,
        // and still above the ceiling.
        let after = test_pool(1_005_000_000, 280_000_000);

        expect_error(
            assert_utilisation(&after, &UtilisationCheck::Ceiling),
            PerpsError::UtilizationTooHigh,
        );
        assert_utilisation(
            &after,
            &UtilisationCheck::NotWorsened {
                reserved_quote: before.reserved_quote,
                quote_deposited: before.quote_deposited,
            },
        )
        .expect("a close that improves utilisation must never revert");
    }

    /// The monotone form is not a rubber stamp: a state that worsens utilisation
    /// still fails it.
    #[test]
    fn the_monotone_form_still_rejects_a_worsening() {
        let before = test_pool(1_000_000_000, 100_000_000);
        let check = |pool: &Pool| {
            assert_utilisation(
                pool,
                &UtilisationCheck::NotWorsened {
                    reserved_quote: before.reserved_quote,
                    quote_deposited: before.quote_deposited,
                },
            )
        };

        check(&test_pool(1_000_000_000, 100_000_000)).expect("unchanged is not worsened");
        check(&test_pool(1_000_000_001, 100_000_000)).expect("more equity is not worsened");
        expect_error(
            check(&test_pool(1_000_000_000, 100_000_001)),
            PerpsError::UtilizationTooHigh,
        );
        expect_error(
            check(&test_pool(999_999_999, 100_000_000)),
            PerpsError::UtilizationTooHigh,
        );
    }

    /// The ceiling still binds where it is supposed to: on the paths that raise
    /// utilisation. Weakening I2 for exits must not weaken it for entries.
    #[test]
    fn the_ceiling_still_binds_on_the_paths_that_raise_utilisation() {
        assert_utilisation(
            &test_pool(1_000_000_000, 200_000_000),
            &UtilisationCheck::Ceiling,
        )
        .expect("exactly at the ceiling is within it");

        // One base unit past it is not, and the comparison is the exact rational
        // rather than a floored bps figure — floored, this would still read
        // 2 000 and pass.
        expect_error(
            assert_utilisation(
                &test_pool(1_000_000_000, 200_000_001),
                &UtilisationCheck::Ceiling,
            ),
            PerpsError::UtilizationTooHigh,
        );
    }

    /// **I3.** A market may never hold a larger slice than the pool's total.
    ///
    /// Both legs are asserted separately, so a copy-paste error comparing
    /// `locked_quote` twice would be caught.
    #[test]
    fn a_market_slice_may_not_exceed_the_pool_total() {
        let mut pool = test_pool(1_000_000_000, 500);
        pool.locked_quote = 900;
        let mut market = test_market();
        market.locked_quote = 900;
        market.reserved_quote = 500;
        assert_market_invariants(&pool, &market).expect("equal slices are legal");

        let mut over_locked = market.clone();
        over_locked.locked_quote = 901;
        expect_error(
            assert_market_invariants(&pool, &over_locked),
            PerpsError::MarketSliceExceedsPool,
        );

        let mut over_reserved = market.clone();
        over_reserved.reserved_quote = 501;
        expect_error(
            assert_market_invariants(&pool, &over_reserved),
            PerpsError::MarketSliceExceedsPool,
        );
    }

    /// **I4, the construction test.** A side has open interest if and only if it
    /// has positions.
    ///
    /// Unreachable in correct operation — open interest is added and subtracted
    /// at entry notional, so the two move together — which is exactly why it
    /// needs deliberate desynchronisation to execute at all. Each of the four
    /// ways to break it is asserted separately: the biconditional has two sides
    /// per leg, and a predicate checking only one would pass a test checking
    /// only the other.
    #[test]
    fn desynchronised_open_interest_is_caught() {
        let pool = test_pool(1_000_000_000, 0);
        assert_market_invariants(&pool, &test_market()).expect("an empty book agrees with itself");

        let breakages: [fn(&mut Market); 4] = [
            |m| m.long_positions = 1,
            |m| m.long_oi_usd = 1,
            |m| m.short_positions = 1,
            |m| m.short_oi_usd = 1,
        ];
        for break_it in breakages {
            let mut drifted = test_market();
            break_it(&mut drifted);
            expect_error(
                assert_market_invariants(&pool, &drifted),
                PerpsError::OpenInterestAccountingDrift,
            );
        }

        // And a book that is merely busy, rather than drifted, is not flagged.
        let mut healthy = test_market();
        healthy.long_positions = 3;
        healthy.long_oi_usd = 900;
        healthy.short_positions = 1;
        healthy.short_oi_usd = 100;
        assert_market_invariants(&pool, &healthy).expect("a consistent book is not drift");
    }
}
