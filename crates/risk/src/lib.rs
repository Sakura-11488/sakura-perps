//! Risk and margin mathematics for Sakura Perps.
//!
//! This crate has **no Solana dependencies**, deliberately. Everything here can
//! be compiled and property-tested in milliseconds, and audited without reading
//! a single line of account plumbing. The program calls into it; it never calls
//! back.
//!
//! That separation is the point. In an oracle-and-pool perpetuals venue, almost
//! every way to lose money is a mistake in this crate — a rounding direction
//! that favours the user, an intermediate that overflows, a margin requirement
//! computed from the wrong notional. Account validation matters too, but it
//! fails loudly. Arithmetic fails silently.
//!
//! # Rules that hold everywhere
//!
//! - **No floating point.** Not anywhere, not for display, not "just for this
//!   ratio". Fixed-point integers throughout. See [`scale`].
//! - **No saturating arithmetic in value-carrying maths.** Saturation destroys
//!   the information that something went wrong at exactly the moment it matters.
//!   Every operation is checked and returns [`RiskError`].
//! - **Rounding always favours the pool**, never the individual user. Stated
//!   once in [`scale`] and applied by choosing `floor` or `ceil` deliberately at
//!   every call site.
//! - **Nothing panics.** No unwraps, no indexing, no `10u128.pow`. Every
//!   fallible operation returns a `Result`.
//!
//! # Module map
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`scale`] | fixed-point scales and the rounding policy |
//! | [`math`] | checked primitives, including 256-bit `mul_div` |
//! | [`position`] | notional, PnL, equity, margin, liquidation price |
//! | [`pool`] | LP share pricing, utilisation, withdrawal safety |
//! | [`funding`] | funding and borrow index accrual |
//! | [`oracle`] | price validation: staleness, confidence, sanity bands |
//! | [`error`] | [`RiskError`] |

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod error;
pub mod funding;
pub mod math;
pub mod oracle;
pub mod pool;
pub mod position;
pub mod scale;

pub use error::RiskError;
pub use position::Side;
