//! Errors this crate can produce.
//!
//! Deliberately not an Anchor `#[error_code]`: this crate has no Solana
//! dependency, so it stays compilable and testable in milliseconds. The program
//! maps these onto its own error enum at the boundary.

/// Everything that can go wrong in risk arithmetic.
///
/// Every variant is a refusal to produce a wrong number. None of them indicate
/// a corrupted state — they are all "this computation cannot be performed
/// correctly, so it will not be performed at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskError {
    /// An intermediate or result exceeded the range of its type.
    MathOverflow,
    /// A divisor was zero.
    DivisionByZero,
    /// A value that must be non-negative was negative.
    NegativeAmount,
    /// A price was zero, or otherwise outside the range a market will accept.
    InvalidPrice,
    /// A position had zero size where a non-zero size is required.
    ZeroSize,
    /// A basis-point parameter exceeded 10 000.
    InvalidBasisPoints,
    /// The pool holds no shares, so a share price cannot be computed.
    EmptyPool,
    /// Initial margin does not exceed maintenance margin plus the liquidation
    /// fee, which would make a freshly opened position immediately liquidatable.
    InvalidMarginParameters,
    /// The oracle price is older than the caller permits, by wall-clock time or
    /// by slots since it landed on chain.
    StalePrice,
    /// The oracle price carries a confidence interval too wide to trade on.
    ConfidenceTooWide,
    /// The oracle price is outside the sanity band configured for this market.
    PriceOutOfBand,
    /// The oracle's exponent is not the one recorded when the feed was
    /// qualified, so every price it produces would be silently rescaled.
    UnexpectedExponent,
    /// The oracle price claims to have been published in the future.
    PriceFromTheFuture,
}

impl core::fmt::Display for RiskError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::MathOverflow => "arithmetic overflow",
            Self::DivisionByZero => "division by zero",
            Self::NegativeAmount => "amount must be non-negative",
            Self::InvalidPrice => "price is zero or out of range",
            Self::ZeroSize => "position size must be non-zero",
            Self::InvalidBasisPoints => "basis points exceed 10000",
            Self::EmptyPool => "pool has no shares outstanding",
            Self::InvalidMarginParameters => {
                "initial margin must exceed maintenance margin plus liquidation fee"
            }
            Self::StalePrice => "oracle price is too old",
            Self::ConfidenceTooWide => "oracle confidence interval is too wide to trade on",
            Self::PriceOutOfBand => "oracle price is outside the configured sanity band",
            Self::UnexpectedExponent => "oracle exponent differs from the qualified value",
            Self::PriceFromTheFuture => "oracle price is published in the future",
        };
        f.write_str(message)
    }
}
