# Risk Rooms — experimental paper kernel

This crate is an **original, unaudited, in-memory paper simulator**. It contains no Solana program, wallet, private key, signature verifier, token transfer, RPC client or deployment path. The asserted owner/delegate IDs and oracle inputs are trusted scenario inputs. Passing a test does not authenticate a user or establish a production financial guarantee.

The product hypothesis is collaborative strategy execution without a shared discretionary wallet: several members can authorize one delegate, while each member retains their own lifetime contribution budget, loss budget and withdrawal rights. A strategy selects a backing cell that both the member and backing owner explicitly permit. That cell alone reserves the position's entire possible payout. Two teams can coordinate strategies without implicitly underwriting each other.

These are **finite, capped, quote-settled contracts**, with optional early close and a fixed maturity observation. They are not perpetual futures: there is no funding, continuous mark accrual, order book, liquidation, portfolio margin, ADL, insurance waterfall or live price discovery. Quote atoms are an abstract six-decimal unit; there is no assumed dollar peg and no real-world maximum-loss guarantee.

## Run the working model

From the repository root:

```sh
cargo test -p sakura-perps-rooms --locked
cargo run -p sakura-perps-rooms --example community_season --locked
cargo clippy -p sakura-perps-rooms --all-targets --locked -- -D warnings
```

The example creates Dawn and Dusk, four individually owned member budgets, two delegates and two independently owned backing cells. It executes three contracts, rejects unauthorized withdrawals/top-ups/policy changes, rejects a mismatched intent revision and replay, blocks stale and missing terminal oracle data, settles capped gains and losses, returns funds to the stored owner, and replays the complete successful transcript. It revokes a delegate before permissionless maturity settlement and checks that the owner's trading nonce remains unchanged. Every adverse expectation is asserted; regressions exit unsuccessfully.

## Consent and budgets

`OwnerId`, `DelegateId`, `MemberId`, `CellId`, `MarketId`, `PositionId` and `RoomId` are distinct types. `Actor` is an asserted role; a real integration must authenticate it independently. Members and cells cannot be deleted/recreated in this prototype. Their IDs cannot silently acquire a new owner.

An entry intent binds the room, member incarnation, exact member and cell policy revisions, market generation, next nonce, issue and expiry slots, nonzero execution-price bounds, allowed cell, direction, size, collateral, profit cap, fee ceiling and maturity. Nonces advance exactly once on successful authorized open/close and never on error. Permissionless maturity settlement consumes the position but no trading nonce. The room rejects decreasing command execution slots. Valid historical observations may have earlier publication slots. It assumes the external simulation clock itself is truthful.

Owners alone contribute, withdraw and replace policy/delegate assignments. A shared delegate may open and close under each member's bounds; it cannot transfer another member's funds into its own account. Withdrawals have no arbitrary destination parameter and return an output addressed only to the stored owner. Cell funding/withdrawal and underwriting changes likewise require that cell's owner. The room administrator registers successive market generations and supplies immutable terminal observations. That is a centralized, trusted price-input role; it does not authorize member policy changes or withdrawals.

Two independent budgets are monotonic:

```text
external contributions used = sum of all successful external member deposits
loss budget used             = all entry fees + all realized gross losses
currently encumbered loss    = sum of open position collateral

new entry requires:
loss budget used + encumbered loss + new collateral + new fee
    <= min(policy loss limit, actual cumulative external contributions)
```

Withdrawals and gains never reduce either used counter. Two simultaneous positions cannot each spend the same unused loss allowance. A successful owner policy change can explicitly increase a future cap, but retains every already-used counter and advances the revision. A lowered loss, notional or position limit applies to future entries; it does not rewrite existing contracts or block their exit. Contribution limits cannot be lowered below contributions already used.

Policy also bounds per-position and total entry notional, exact entry leverage, open-position count, fees, intent lifetime, market IDs and cell IDs. Free collateral must actually cover the debit. Leverage uses an exact wide-product comparison; it does not accept a fraction above the cap because division rounded down. The prototype does not increase a loss allowance merely because paper profits increased free collateral.

## Fully reserved cell accounting

Each cell has a versioned `UnderwritingPolicy`. Its owner admits exact `(MemberId, OwnerId, generation)` tuples and exact market generations, and bounds per-position notional, leverage and profit cap, total reserved payouts, and a minimum entry fee. Forward admissions bind the intended owner, so claiming an unused member ID does not acquire somebody else's admission. Empty admission sets pause new entries. Only the cell owner can revise these terms. Future-entry changes never release active reserves or rewrite accepted receipts; even lowering the reserve ceiling below current obligations does not weaken those obligations. The member's own allowlist alone cannot expose another owner's backing.

For a contract with collateral `C`, fixed entry fee `F`, and maximum profit `P`:

```text
member free balance decreases by C + F
chosen cell balance increases by C + F
chosen cell reserve increases by C + P
P must fit that cell's free backing before the new entry fee

payout = clamp(C + price PnL, 0, C + P)
```

The lower/upper payout bounds define the contract itself. An adverse move beyond collateral creates no extra debt; a favorable move beyond the profit cap creates no additional claim. This differs economically from an uncapped perp, including when a continuous market would liquidate the position. Entry fees are final and there are no exit fees or mutable carry charges. Losses and fees remain in the chosen cell; its owners bear capped profits. There is no cell-to-cell bailout or transfer command.

Every successful action reconciles:

```text
all external contributions
    = member free balances + cell balances + all external withdrawals

each cell reserved balance = sum of its open contracts' maximum payouts
each cell reserved balance <= that cell's balance
```

Member open collateral/notional/count and immutable owner attribution are checked too. Reserved balances cannot be withdrawn by the cell owner. Prices in an admitted market's full configured range are preflighted at entry so an in-band close cannot newly fail because PnL/equity exceeds the numeric domain. All value arithmetic is checked. The existing dependency `sakura-perps-risk` provides wide integer math, PnL and capped settlement; its six-decimal numerical units are interpreted here as abstract quote units, not dollar valuations. No upstream Percolator source was copied.

## Price, generation and maturity assumptions

Prices use `PRICE_SCALE = 10^10`; base size uses the market's declared decimals, at most 18. Entries and early closes check positive configured price bands, exact market/generation binding, no future observation and maximum observation age. User execution bounds are checked separately. Oracle ownership, independent price sources, confidence intervals, market liquidity and manipulation resistance are not modeled.

Every position snapshots its market, member policy and backing policy. Registering a new market generation or changing policy cannot alter that position's fees, reserve, payoff or maturity. Authorized close intent binds the original generation/revision and the member's current nonce; current delegate authorization still applies, so revocation takes effect immediately. An owner may submit a valid close despite revoking the delegate, subject to the oracle/time gates.

At or after maturity, the asserted administrator records one immutable observation published **exactly at the captured maturity slot**. It is keyed by `(market, generation, maturity slot)`, shared across matching contracts, and checked against the original position's market snapshot. An old generation's observation remains publishable after market rotation. That historical observation may arrive later and is checked at maturity rather than treated as a stale live price. Attempts to replace it fail. This constrains replay consistency; it does not authenticate or establish the truth of the administrator's price.

Anyone may then issue `SettleMatured(position, now_slot)`. It takes no actor, beneficiary, price, consent or nonce argument; it pays the stored member using the stored terminal observation and removes that position exactly once. It works after the trader disappears or changes member/delegate/cell policy. An authorized mature close must use that same observation. Until the terminal observation arrives, settlement fails atomically and the reserve remains locked. Missing terminal data is an **unresolved lockup/recovery policy gate**: there is no last-price fallback or guaranteed recovery through a permanent oracle outage. A real contract design needs a reviewed settlement-oracle and outage-resolution mechanism before deployment.

## Atomicity, receipts and replay

`Room::execute` clones, plans, checks invariants, appends a successful record, then commits. Any returned error leaves the original state and transcript unchanged, including late arithmetic failures and nonce updates. This is explicit transactional behavior for the in-memory model, not an assumption that arbitrary Rust calls roll back.

An `OpenReceipt` freezes the entire executed intent, canonical member and underwriting policy fields/revisions, market generation/configuration, observation, actual entry notional, fee, collateral, cap, maturity, consent expiry, member owner and backing owner. `payout_at` recomputes its economic payoff without current room policy. A `CloseReceipt` records actual payout, loss and released reserve. Callers receive copies or immutable references; they cannot mutate the archived records.

`Room::replay` reexecutes the complete command transcript from genesis and checks every output. It detects a changed receipt. It does **not** make a jointly rewritten command/receipt history authentic. There are no cryptographic commitments or signed bytes. `inspection_bytes` uses deterministic Rust Debug formatting solely for same-version unchanged-state tests; it is explicitly not a stable wire codec, signing format or persistence format.

Clone/commit and full-state checks are intentionally expensive and grow with the transcript. This is a small research workbench, not a bounded-compute on-chain architecture. No claims are made about throughput, fork safety, crash durability or concurrency.

## Evidence and remaining work

The 17 tests cover exact atom conservation, owner/delegate scope, cumulative external and loss budgets, simultaneous risk reservation, wrong nonce/revision/generation/room, stale/future/missing maturity observations, revoked delegation, immutable existing economics, cross-cell isolation, reserve withdrawal refusal, late-overflow rollback, tampered receipt replay and exact leverage limits. They also cover backing-owner admission, preclaimed identity attacks, underwriting limits/revisions, immutable terminal publication after market rotation, permissionless fixed-beneficiary settlement and repeat settlement rejection without nonce consumption. The generated campaign exercises 32 seeds × 64 commands. Payout monotonicity is checked on 991 prices for each direction. All 24 close orders of four funded positions produce the same balances at a fixed price.

These are finite regression and state-machine checks, not formal proofs or exhaustive attack discovery. A live implementation would additionally need authenticated identities and typed signing, durable canonical serialization, authenticated current and historical oracle adapters, settlement outage resolution, custody/account validation, economic review of capped payoffs, and independent security review. The broader repository and researched Percolator programs are not made production-ready by this crate.
