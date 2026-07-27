//! In-process SVM tests for the oracle path.
//!
//! These cover the one thing the risk crate's property tests cannot: whether a
//! genuine `PriceUpdateV2` account, owned by the real Pyth receiver program and
//! serialised exactly as the receiver writes it, deserialises into the five
//! numbers validation expects. Everything downstream of that is already proven
//! exhaustively in pure Rust.
//!
//! The reason this is LiteSVM rather than `solana-program-test` is
//! [`LiteSVM::set_account`]. Writing arbitrary bytes into an account turns
//! scenarios that are otherwise unreachable — a feed that stopped updating, a
//! confidence interval that widened mid-dislocation, a timestamp from the future
//! — into three lines that run in milliseconds. Waiting for a real price to
//! move is not a testing strategy.
//!
//! # Running these
//!
//! They need the compiled program, which this repository builds in CI:
//!
//! ```text
//! SAKURA_PERPS_SO=target/deploy/sakura_perps.so cargo test -p sakura-perps-svm-tests
//! ```
//!
//! Absent the binary they skip with a notice rather than failing, so a developer
//! without a Solana toolchain still gets a green `cargo test`. CI asserts the
//! file exists before invoking them, so they cannot silently skip there — a
//! skipped safety test that reports success is worse than no test at all.

use anchor_lang::{AnchorSerialize, Discriminator, InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel};
use sakura_perps::PerpsError;
use solana_account::Account;
use solana_address::Address;
use solana_clock::Clock;
use solana_instruction::{error::InstructionError, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

/// Feed id used throughout. Arbitrary but fixed, so a mismatch is deliberate.
const FEED_ID: [u8; 32] = [7u8; 32];

/// Cluster time the tests pin the clock to, so staleness is exact rather than
/// dependent on when the suite happens to run.
const NOW_UNIX: i64 = 1_800_000_000;
/// Cluster slot the tests pin.
const NOW_SLOT: u64 = 100_000;

/// $123.45 at Pyth's usual crypto exponent of -8.
const PRICE_MANTISSA: i64 = 12_345_000_000;
const PRICE_EXPONENT: i32 = -8;

/// Load the compiled program, or fail loudly.
///
/// # Why this panics rather than skipping
///
/// It skipped, once, and the skip returned `Ok(())`. That meant
/// `a_good_price_is_accepted` — whose entire assertion is `is_ok()` — **passed
/// while executing nothing at all**. Eight sibling tests failed with "expected
/// OracleStale, got Ok(())", and only that noise revealed the happy-path test
/// was green for no reason.
///
/// The skip existed so a developer without a Solana toolchain would still get a
/// green `cargo test`. Wrong trade, twice over: this crate cannot build without
/// the Agave runtime anyway, so that green was never achievable — and a safety
/// test that reports success without running is worse than one that does not
/// exist. Absence is visible; false confidence is not.
///
/// The path resolves against the workspace root, not the process working
/// directory, because `cargo test` runs with the *crate* directory as cwd. A
/// relative `target/deploy/...` therefore pointed at
/// `crates/svm-tests/target/deploy/...`, while the CI step asserting the file
/// existed checked the workspace root. Two different paths, one always missing.
fn program_binary() -> Vec<u8> {
    let path = std::env::var("SAKURA_PERPS_SO")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/deploy/sakura_perps.so")
        });

    std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read the program binary at {}: {err}\n\
             Build it with `anchor build`, or point SAKURA_PERPS_SO at an \
             existing .so. These tests deliberately do not skip: a safety test \
             that reports success without running is worse than no test.",
            path.display()
        )
    })
}

/// Assemble a `PriceUpdateV2` account exactly as the Pyth receiver writes it:
/// eight-byte Anchor discriminator, then the borsh body, owned by the receiver.
fn price_update_account(
    feed_id: [u8; 32],
    mantissa: i64,
    confidence: u64,
    exponent: i32,
    publish_time: i64,
    posted_slot: u64,
) -> Account {
    let update = PriceUpdateV2 {
        write_authority: Address::new_unique(),
        // Anything less than Full and `get_price_no_older_than` rejects it
        // outright — which is a behaviour worth relying on, not working around.
        verification_level: VerificationLevel::Full,
        price_message: pyth_solana_receiver_sdk::price_update::PriceFeedMessage {
            feed_id,
            price: mantissa,
            conf: confidence,
            exponent,
            publish_time,
            prev_publish_time: publish_time - 1,
            ema_price: mantissa,
            ema_conf: confidence,
        },
        posted_slot,
    };

    let mut data = PriceUpdateV2::DISCRIMINATOR.to_vec();
    update
        .serialize(&mut data)
        .expect("PriceUpdateV2 serialises");

    Account {
        lamports: 1_000_000_000,
        data,
        // Ownership is what makes this a Pyth account rather than 200 bytes of
        // wishful thinking. `Account<PriceUpdateV2>` checks it.
        owner: pyth_solana_receiver_sdk::ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// Guard values matching `OracleGuards::for_trading`, spelled out so a test can
/// vary exactly one of them.
struct Guards {
    max_age_seconds: u32,
    max_age_slots: u64,
    max_future_skew_seconds: u32,
    max_confidence_bps: u16,
    min_price: u128,
    max_price: u128,
    expected_exponent: i32,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            max_age_seconds: 30,
            max_age_slots: 150,
            max_future_skew_seconds: 5,
            max_confidence_bps: 100,
            min_price: 1,
            max_price: u128::MAX,
            expected_exponent: PRICE_EXPONENT,
        }
    }
}

/// Run `probe_oracle` against a given account and guards.
fn probe(account: Account, feed_id: [u8; 32], guards: Guards) -> Result<(), TransactionError> {
    let binary = program_binary();

    let mut svm = LiteSVM::new();
    svm.add_program(sakura_perps::ID, &binary)
        .expect("program loads into the SVM");

    // Pin the clock. Staleness is measured against this, so the assertions are
    // exact rather than a function of when the suite ran.
    let mut clock: Clock = svm.get_sysvar();
    clock.unix_timestamp = NOW_UNIX;
    clock.slot = NOW_SLOT;
    svm.set_sysvar(&clock);

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000)
        .expect("airdrop");

    let price_update = Address::new_unique();
    svm.set_account(price_update, account).expect("set account");

    let instruction = Instruction {
        program_id: sakura_perps::ID,
        accounts: sakura_perps::accounts::ProbeOracle { price_update }.to_account_metas(None),
        data: sakura_perps::instruction::ProbeOracle {
            params: sakura_perps::ProbeOracleParams {
                feed_id,
                max_age_seconds: guards.max_age_seconds,
                max_age_slots: guards.max_age_slots,
                max_future_skew_seconds: guards.max_future_skew_seconds,
                max_confidence_bps: guards.max_confidence_bps,
                min_price: guards.min_price,
                max_price: guards.max_price,
                expected_exponent: guards.expected_exponent,
            },
        }
        .data(),
    };

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );

    svm.send_transaction(transaction)
        .map(|_| ())
        .map_err(|failed| failed.err)
}

/// Anchor renumbers `#[error_code]` variants from 6000.
fn expect_error(result: Result<(), TransactionError>, expected: PerpsError) {
    let code = expected as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
    match result {
        Err(TransactionError::InstructionError(0, InstructionError::Custom(actual))) => {
            assert_eq!(
                actual, code,
                "wrong error: expected {expected:?} ({code}), got {actual}"
            );
        }
        other => panic!("expected {expected:?}, got {other:?}"),
    }
}

/// A well-formed, recent, confident price is accepted.
#[test]
fn a_good_price_is_accepted() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        1_000_000, // 0.008% of price, well inside 100 bps
        PRICE_EXPONENT,
        NOW_UNIX - 5,
        NOW_SLOT - 10,
    );
    assert!(
        probe(account, FEED_ID, Guards::default()).is_ok(),
        "a fresh, confident price should be accepted"
    );
}

/// A price older than the market permits is refused, by upstream timestamp.
#[test]
fn a_stale_price_is_refused() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        0,
        PRICE_EXPONENT,
        NOW_UNIX - 3_600,
        NOW_SLOT - 10,
    );
    // The SDK's own age check fires first here, which is the correct layering:
    // it refuses before our validation is even reached.
    let result = probe(account, FEED_ID, Guards::default());
    assert!(result.is_err(), "an hour-old price must not be accepted");
}

/// A price whose timestamp is fresh but which landed on chain long ago is
/// refused. This is the case a single-clock implementation misses entirely.
#[test]
fn a_price_stale_by_slots_is_refused_despite_a_fresh_timestamp() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        0,
        PRICE_EXPONENT,
        NOW_UNIX, // upstream says: brand new
        NOW_SLOT - 10_000,
    );
    expect_error(
        probe(account, FEED_ID, Guards::default()),
        PerpsError::OracleStale,
    );
}

/// A price published in the future is refused rather than treated as infinitely
/// fresh. A naive `now - publish_time > limit` check computes a negative age
/// here, passes, and accepts the price forever.
#[test]
fn a_future_price_cannot_bypass_staleness() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        0,
        PRICE_EXPONENT,
        NOW_UNIX + 86_400,
        NOW_SLOT,
    );
    expect_error(
        probe(account, FEED_ID, Guards::default()),
        PerpsError::OraclePriceFromTheFuture,
    );
}

/// A confidence interval wider than the market tolerates is refused.
#[test]
fn a_wide_confidence_interval_is_refused() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        // 2% of price, against a 100 bps limit.
        (PRICE_MANTISSA / 50) as u64,
        PRICE_EXPONENT,
        NOW_UNIX - 1,
        NOW_SLOT - 1,
    );
    expect_error(
        probe(account, FEED_ID, Guards::default()),
        PerpsError::OracleConfidenceTooWide,
    );
}

/// The same degraded price is accepted under liquidation thresholds. A position
/// that cannot be closed while the oracle is briefly uncertain keeps losing
/// money, and the pool absorbs it.
#[test]
fn liquidation_thresholds_accept_what_trading_thresholds_refuse() {
    let wide = || {
        price_update_account(
            FEED_ID,
            PRICE_MANTISSA,
            (PRICE_MANTISSA / 50) as u64,
            PRICE_EXPONENT,
            NOW_UNIX - 1,
            NOW_SLOT - 1,
        )
    };

    expect_error(
        probe(wide(), FEED_ID, Guards::default()),
        PerpsError::OracleConfidenceTooWide,
    );

    let liquidation = Guards {
        max_age_seconds: 60,
        max_age_slots: 300,
        max_confidence_bps: 500,
        ..Guards::default()
    };
    assert!(
        probe(wide(), FEED_ID, liquidation).is_ok(),
        "liquidation must remain possible on a price too uncertain to open against"
    );
}

/// A feed that has changed exponent is refused before anything is scaled by it.
/// Accepting it would produce prices wrong by a power of ten that sail straight
/// through a sanity band chosen for the old scale.
#[test]
fn a_changed_exponent_is_refused() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        0,
        -6, // feed now publishes -6; the market was qualified at -8
        NOW_UNIX - 1,
        NOW_SLOT - 1,
    );
    expect_error(
        probe(account, FEED_ID, Guards::default()),
        PerpsError::OracleExponentChanged,
    );
}

/// A price outside the market's sanity band is refused.
#[test]
fn a_price_outside_the_sanity_band_is_refused() {
    let account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        0,
        PRICE_EXPONENT,
        NOW_UNIX - 1,
        NOW_SLOT - 1,
    );
    let narrow = Guards {
        // $123.45 sits well below a $1000 floor.
        min_price: 1_000 * 10_000_000_000,
        max_price: 2_000 * 10_000_000_000,
        ..Guards::default()
    };
    expect_error(
        probe(account, FEED_ID, narrow),
        PerpsError::OraclePriceOutOfBand,
    );
}

/// An account for a different feed is refused. This is what stops a price for
/// one asset being presented to another asset's market — the single most direct
/// oracle substitution attack.
#[test]
fn an_account_for_a_different_feed_is_refused() {
    let account = price_update_account(
        [9u8; 32], // account carries a different feed
        PRICE_MANTISSA,
        0,
        PRICE_EXPONENT,
        NOW_UNIX - 1,
        NOW_SLOT - 1,
    );
    expect_error(
        probe(account, FEED_ID, Guards::default()),
        PerpsError::OraclePriceUnavailable,
    );
}

/// An account owned by something other than the Pyth receiver is refused at
/// deserialisation. Convincing-looking bytes are not enough.
#[test]
fn an_account_not_owned_by_the_pyth_receiver_is_refused() {
    let mut account = price_update_account(
        FEED_ID,
        PRICE_MANTISSA,
        0,
        PRICE_EXPONENT,
        NOW_UNIX - 1,
        NOW_SLOT - 1,
    );
    account.owner = Address::new_unique();

    let result = probe(account, FEED_ID, Guards::default());
    assert!(
        result.is_err(),
        "an account not owned by the Pyth receiver must be refused"
    );
}
