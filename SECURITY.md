# Security Policy

## Current status

**This code is unaudited, deployed only to devnet, and holds nothing of value.**

There is no bug bounty yet. There has been no third-party audit. Do not use this
software with funds of real economic value, and do not treat anything here as
production-ready.

We would still very much like to hear about problems you find.

## Reporting a vulnerability

Email **security@sakuraonseeker.com** with:

- what the issue is, and which file or instruction it affects
- how to reproduce it, ideally as a failing test or a devnet transaction
- what an attacker gains

Please do not open a public GitHub issue for anything that looks exploitable.

**Response commitment:** acknowledgement within 3 working days, and an initial
assessment within 10. If we disagree that something is a vulnerability we will
say so and explain why, rather than letting the report go quiet.

## Scope

In scope:

- everything under `programs/`
- the deployment and upgrade-authority configuration described in the README
- the TypeScript client and tests, where a flaw there could mislead a user into
  signing something harmful

Out of scope, for now:

- denial of service against public RPC endpoints
- issues that require a compromised admin key, unless the point of the report is
  that the admin can do something the documentation claims they cannot
- anything in the `archive/fee-router-v0` tag, which is retained for provenance
  only and is known to be broken

## Safe harbour

We will not pursue legal action against anyone who, in good faith:

- tests only against devnet or their own local validator
- avoids accessing, modifying, or destroying data belonging to others
- gives us reasonable time to respond before publishing

If you are unsure whether something is in scope, ask first — we would rather
answer a question than receive a report about production.

## What we already know

Stated plainly, so nobody spends time rediscovering it:

- Market listing is permissionless, but the **oracle allowlist is admin-only, by
  design**. Permissionless feed qualification would be equivalent to no gate at
  all, because feed creation on some oracle networks is itself permissionless
  with arbitrary jobs.
- A newly created market opens quarantined with zero open-interest allowance
  until an admin sets its risk parameters.
- The predecessor code in this repository — a fee router and a crank bot,
  preserved under the tag `archive/fee-router-v0` — never compiled, never
  deployed, and contained a crank bot whose staleness check was
  `Math.random() < 0.2`. It is not part of this project. Please do not report
  bugs in it; report them in code we intend to run.
