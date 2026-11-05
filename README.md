# Sakura Perps

**Plan together. Decide for yourself.**

Sakura explores community strategy rooms where each participant controls their
own allocation and risk. This repository contains the Rooms paper experience,
its accounting and authorization engines, and a separate Solana perpetuals
research program.

[Try the invite-only paper beta](https://sakuraonseeker.com/app/perps/community/)
· [Documentation](docs/README.md) · [Roadmap](ROADMAP.md)
· [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

**Unaudited. No real funds.** The hosted beta uses simulated credits. The Solana
program is devnet research and is not approved for mainnet or real-value assets.

## What exists today

Status recorded **5 November 2026**. Source, test evidence and deployment are
separate: merging code does not update the hosted website or on-chain program.

| Component | Current state | Boundary |
|---|---|---|
| [Community Rooms](docs/community-rooms-beta.md) | Invite-only hosted paper beta, released 4 November | Separate browser identities, signed allocations and durable shared history; a fixed scenario, no deposits or live prices |
| [Local Rooms](docs/rooms-paper-pilot.md) | Single-device paper pilot | Rust WASM accounting and IndexedDB recovery; clearing site data loses signing access |
| [Risk Rooms](crates/rooms/README.md) | Experimental finite-contract kernel with a [signed gateway](crates/rooms-auth/README.md) | Explicit budgets and separately reserved backing; no token custody or trusted live oracle |
| [Solana program](programs/sakura-perps/src/lib.rs) | Oracle-and-pool prototype; latest recorded deployment is `devnet-v0.7.0` | Source hardening is newer than that deployment; LP exits, economic policy and independent assurance remain open |
| [Keeper](keeper/README.md) and [dashboard](dashboard/README.md) | Devnet operational tooling and read-only monitoring | Sustained funded-book operation and pinned-feed refresh still need evidence |

The app's existing Phoenix integration is separate from this research program.
Rooms does not establish that the Solana protocol is integrated or ready for funds.

## The Rooms experience

1. **Plan:** join an immutable community strategy plan using a single-use invite.
2. **My allocation:** approve your own paper allocation. A leader cannot sign for you.
3. **Review:** compare outcomes against the same scenario and retain signed history.

The current exercise uses prices **100 → 92 → 104**, 1–100 whole paper credits
and 1–3× long/short exposure. The midpoint does not execute stops or liquidations.
These are finite, capped contracts, not a live perpetual market or trading record.

The service persists accepted actions in SQLite and the browser retains pending
requests and verified checkpoints. Recovery tests cover interrupted delivery,
restarts, invitation races and revocation. The operator and delivered JavaScript
remain trusted; signing keys are not recoverable from public exports. Read the
[trust and limits](docs/community-rooms-beta.md#trust-and-limits) before testing.

## Quickstart

Explore the paper model locally with **Rust 1.89.0** installed through rustup:

```sh
git clone https://github.com/Sakura-11488/sakura-perps.git
cd sakura-perps
scripts/security-challenge.sh paper
```

This runs the Rust Rooms kernels, signed-command checks and examples without a
wallet, RPC configuration or transactions. Dependency downloads need network
access. It does not start the hosted service or launch a public security challenge.

For the web experience, use the [browser integration guide](integrations/sakura-rooms/README.md).
For a private local server, use the [community service runbook](community/README.md).

## Repository map

| Path | Purpose |
|---|---|
| [`community/`](community/README.md) | Signed HTTP protocol, durable SQLite service and operations |
| [`crates/rooms/`](crates/rooms/README.md) | Finite-contract paper accounting and backing rules |
| [`crates/rooms-auth/`](crates/rooms-auth/README.md) | Ed25519 commands, authority and replay checks |
| [`crates/rooms-web/`](crates/rooms-web/README.md) | Canonical browser facade compiled to WASM |
| [`integrations/sakura-rooms/`](integrations/sakura-rooms/README.md) | Allowlisted Expo UI/client source and shipped engine assets |
| [`programs/sakura-perps/`](programs/sakura-perps/) and [`crates/risk/`](crates/risk/) | Solana protocol and fixed-point risk mathematics |
| [`keeper/`](keeper/README.md), [`dashboard/`](dashboard/README.md), [`idl/`](idl/) | Operational tools and generated client ABI |
| [`docs/`](docs/README.md) | Design, research, security evidence and historical references |

The complete application lives in [Sakura](https://github.com/Sakura-11488/Sakura).
This repository publishes only the allowlisted Rooms integration; private Studio
source and operator credentials do not belong here. A web release must preserve
Studio, ATSU restoration and the community service; an app export alone is insufficient.

## Toolchain

| Component | Supported version |
|---|---|
| Host Rust | 1.89.0, pinned in `rust-toolchain.toml` |
| Anchor CLI and Rust libraries | 1.1.2 |
| Agave / SBF platform-tools | 4.1.2 / v1.54 |
| Node | 22; community service pinned to 22.23.x |
| WASM target / bindings CLI | `wasm32-unknown-unknown` / 0.2.126 |

Use both rustup's `cargo` and `rustc`. See the
[detailed toolchain reference](docs/reference/protocol-development.md#toolchain)
for compiler setup and compatibility notes.

## Testing

[CI](https://github.com/Sakura-11488/sakura-perps/actions/workflows/ci.yml) covers
seven jobs: keeper/dashboard, Rooms browser/service, lint/secret scanning, risk
and program host tests, SBF/IDL build, compiled-program SVM tests, and guardrails.
The Rooms job checks both shipped and freshly built WASM. Review the result for
the exact commit; a badge or test count is not an audit.

The [contributor guide](CONTRIBUTING.md#validation) lists focused commands.
[`scripts/security-challenge.sh full`](docs/security-challenge.md#start-without-a-wallet)
is the comprehensive local entry point after installing its prerequisites.

## Roadmap

Our immediate phase is a small invited cohort: complete real user sessions,
improve recovery and reporting, and automate encrypted off-host backups with a
restore drill. Realistic market-data paper sessions follow explicit pricing and
outage specifications. Protocol economics and independent assurance proceed as
separate gates. See the [acceptance criteria](ROADMAP.md); no mainnet date is set.

## Security

[SECURITY.md](SECURITY.md) is the source of truth for reporting. A private channel
and assigned responder must be verified before announcing the local research
challenge. There is no independent audit, funded bounty or response-time promise.
Do not publish exploit details in ordinary issues or test shared infrastructure.

## History and license

[Historical protocol development](docs/reference/protocol-development.md) preserves
deployment records, commands, account-layout notes and previous milestones.
[Research](docs/README.md#research-and-provenance) records prior art and remaining
questions; it does not establish novelty or certify security.

[Business Source License 1.1](LICENSE), with the additional use grant stated there.
This is source-available software. Production trading use requires a commercial
license until the stated change date, **27 July 2030**, when it converts to Apache-2.0.
