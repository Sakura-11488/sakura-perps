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
use sakura_perps_risk::math::to_u64;
use sakura_perps_risk::pool as risk_pool;

use crate::{Exchange, PauseFlags, PerpsError};

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
    /// Utilisation ceiling a withdrawal may not push the pool past.
    pub max_utilization_bps: u16,
    /// Hard cap on `quote_deposited`. Essential on the way to mainnet: it bounds
    /// what can be lost while the protocol is still unproven.
    pub max_aum_quote: u64,
    /// `MINIMUM_LIQUIDITY` in this pool's collateral base units.
    ///
    /// Taken from `_reserved` rather than appended, so `INIT_SPACE` is unchanged
    /// and no existing pool needs reallocating. The live devnet pool reads `0`
    /// here, which is inert: the only branch that consumes it is the
    /// `total_shares == 0` first-deposit path, which that pool is long past.
    pub min_liquidity_quote: u64,
    pub _reserved: [u8; 120],
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
    // Strictly below 100%: a ceiling equal to AUM permits reserving every asset
    // the pool has, leaving nothing to absorb the next adverse move, and it is
    // the boundary the floored-utilisation comparison used to leak through.
    require!(
        params.max_utilization_bps < crate::BPS_DENOMINATOR,
        PerpsError::InvalidBasisPoints
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

    assert_vault_solvent(&ctx.accounts.quote_vault, pool)
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
    assert_vault_solvent(&ctx.accounts.quote_vault, pool)
}

/// The invariant from the module docs, asserted for real.
///
/// A comment claiming an invariant holds is a hope. This is the check.
fn assert_vault_solvent(vault: &InterfaceAccount<'_, TokenAccount>, pool: &Pool) -> Result<()> {
    let liabilities = pool
        .quote_deposited
        .checked_add(pool.locked_quote)
        .and_then(|value| value.checked_add(pool.pending_protocol_fees))
        .ok_or(PerpsError::MathOverflow)?;

    require!(vault.amount >= liabilities, PerpsError::VaultInsolvent);
    Ok(())
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
