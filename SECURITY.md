# Security Policy

## Current status

**This code is unaudited and is not approved for real funds.** The public
[security challenge](docs/security-challenge.md) targets local simulation at an
identified source commit. The older shared devnet deployment does not contain
all current source changes and is not the challenge target.

There is no bug bounty yet. There has been no third-party audit. Do not use this
software with funds of real economic value, and do not treat anything here as
production-ready.

We would still very much like to hear about problems you find.

## Reporting a vulnerability

**Private report intake is not configured yet.** The previously listed mailbox
has not been verified as controlled or monitored by the owner and must not be
used for sensitive reports. GitHub private vulnerability reporting is currently
disabled. A repository administrator needs to
[enable it](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/configure-vulnerability-reporting/configure-for-a-repository)
and assign a responder before the challenge is publicly announced.

Until a verified private route is published here, retain exploit details
privately. You may open a public issue asking only for a security contact,
without code, impact details, credentials or reproduction steps.

Once a private channel is available, include:

- what the issue is, and which file or instruction it affects
- how to reproduce it, ideally as a failing local test with the source/build manifest
- what an attacker gains

Please do not open a public GitHub issue for anything that looks exploitable.
There is no response-time commitment until a responder and channel are confirmed.

## Scope

In scope:

- everything under `programs/`
- the risk library, Risk Rooms paper model, keeper and read-only dashboard
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

- tests only on their own local simulator or validator with simulated assets
- avoids accessing, modifying, or destroying data belonging to others
- gives us reasonable time to respond before publishing

If you are unsure whether something is in scope, ask first — we would rather
answer a question than receive a report about production.
This invitation does not authorize attacks on shared infrastructure or grant
permission on behalf of any third party.

## What we already know

The current security work and remaining release gates are tracked in
[Security hardening](docs/security-hardening.md). Source changes there are not
evidence of a deployment or an independent audit.

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
