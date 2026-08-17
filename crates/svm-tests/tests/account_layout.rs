//! The on-chain account lengths are frozen, and a legacy buffer still decodes.
//!
//! Anchor has no migration story. A changed `INIT_SPACE` does not grow an
//! allocated account, it **orphans** it: every already-created instance is the
//! wrong size forever, and there is no instruction in this program that could
//! reallocate one. There are live devnet accounts.
//!
//! `programs/sakura-perps/src/` already asserts each length with a `const _`,
//! which catches a change at compile time. What it cannot catch is the case that
//! actually matters at runtime: whether an account written by the **previous**
//! build — all-zero in the bytes stage 3 carved out of `_reserved` — still
//! deserialises, and still reads as the "never quarantined, never priced,
//! zero-spread" state the program documents those zeros to mean.
//!
//! No LiteSVM here, and no compiled program: this is a Borsh round trip over the
//! same types the runtime uses, so it runs even where the Solana toolchain does
//! not.

use anchor_lang::{AccountDeserialize, AccountSerialize, Discriminator, Space};
use sakura_perps::market::{Market, QualifiedFeed};
use sakura_perps::pool::{Pool, WithdrawRequest};
use sakura_perps::position::Position;
use sakura_perps::Exchange;

/// Decode an all-zero body of exactly `INIT_SPACE` bytes behind the type's own
/// discriminator, then prove the length is pinned from both directions:
/// re-serialising produces exactly `8 + INIT_SPACE`, and one byte fewer fails.
///
/// The short-buffer half is what makes this a length test rather than a smoke
/// test. Anchor's `try_deserialize` ignores trailing bytes, so a buffer that is
/// too **long** decodes happily — only the failure on a short one shows that
/// every declared byte is genuinely read.
fn legacy_zero_account<T>(space: usize) -> T
where
    T: AccountDeserialize + AccountSerialize + Discriminator + Space,
{
    assert_eq!(
        T::INIT_SPACE,
        space,
        "INIT_SPACE is frozen: there are live accounts at the old length, and \
         Anchor orphans rather than grows them"
    );

    let mut data = T::DISCRIMINATOR.to_vec();
    data.resize(8 + space, 0u8);
    let decoded = T::try_deserialize(&mut data.as_slice())
        .expect("an all-zero legacy buffer must still decode");

    let mut round_tripped = Vec::new();
    decoded
        .try_serialize(&mut round_tripped)
        .expect("re-serialises");
    assert_eq!(
        round_tripped.len(),
        8 + space,
        "the serialised length must equal 8 + INIT_SPACE exactly"
    );
    assert_eq!(
        round_tripped, data,
        "an all-zero body round-trips unchanged"
    );

    let short = &data[..data.len() - 1];
    assert!(
        T::try_deserialize(&mut &short[..]).is_err(),
        "a buffer one byte short must fail, or INIT_SPACE is not what is read"
    );

    decoded
}

/// Every frozen length, and the stage-3 fields all reading zero.
///
/// Those three fields on `Market` and the one on `Position` were taken **out of
/// `_reserved`** rather than appended, which is the only reason the lengths are
/// unchanged. Zero is a meaningful value for each: never quarantined, never
/// priced, and — for `spread_bps` — the zero-spread case, at which
/// `execution_price` is total and therefore cannot make an exit unpriceable.
#[test]
fn the_frozen_account_lengths_still_decode_a_legacy_buffer() {
    let exchange: Exchange = legacy_zero_account(304);
    assert_eq!(exchange.paused_flags, 0);

    let pool: Pool = legacy_zero_account(252);
    assert_eq!(pool.max_utilization_bps, 0);
    let _: WithdrawRequest = legacy_zero_account(121);

    let feed: QualifiedFeed = legacy_zero_account(205);
    assert!(!feed.revoked);

    let market: Market = legacy_zero_account(552);
    assert_eq!(
        (
            market.quarantined_ts,
            market.last_good_price,
            market.last_good_price_ts
        ),
        (0, 0, 0),
        "stage 3's market fields came out of _reserved, so a legacy market \
         reads zero for all three"
    );

    let position: Position = legacy_zero_account(240);
    assert_eq!(
        position.spread_bps, 0,
        "a position written before the field existed reads the zero-spread case"
    );
}
