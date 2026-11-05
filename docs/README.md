# Documentation

Start with the [project overview](../README.md) for the current product and
deployment boundaries. The [roadmap](../ROADMAP.md) separates the invited paper
cohort, future realistic simulation, and financial-protocol research.

## Try and understand Rooms

- [Community beta](community-rooms-beta.md): invitations, independent decisions,
  recovery, trust and limits.
- [Single-device pilot](rooms-paper-pilot.md): the local Plan, My allocation and
  Review experience.
- [Public Expo integration](../integrations/sakura-rooms/README.md): source
  packaging, WASM build, browser verification and full `/app/` export tests.
- [Community service runbook](../community/README.md): private local provisioning,
  persistent storage, backup and restore.
- [Wire contract](../community/CONTRACT.md): signatures, canonical requests and replay.

## Contribute and verify

- [Contributor guide](../CONTRIBUTING.md): repository boundaries and checks.
- [Risk Rooms kernel](../crates/rooms/README.md): finite-contract accounting,
  contribution/loss budgets and fully reserved backing cells.
- [Authenticated commands](../crates/rooms-auth/README.md): signed authority and replay.
- [Browser facade](../crates/rooms-web/README.md): canonical paper scenario execution.
- [Keeper](../keeper/README.md) and [dashboard](../dashboard/README.md): devnet tooling.
- [CI workflow](../.github/workflows/ci.yml): exact verification jobs.

## Security

- [Reporting policy](../SECURITY.md): current intake status and permitted research scope.
- [Security hardening](security-hardening.md): implemented controls and unresolved gates.
- [Local challenge](security-challenge.md): wallet-free entry points, build manifests
  and attack hypotheses. Public launch is pending reporting and responder readiness.

The hosted beta contains paper data. That does not authorize attacks on its
shared service or users. Run security experiments against your own local instance.

## Research and provenance

- [Product and market research, 30 October 2026](research/2026-10-30-perps-direction.md).
- [Percolator review and Risk Rooms, 2 November 2026](research/2026-11-02-percolator-and-risk-rooms.md):
  pinned upstream sources, adopted ideas, rejected approaches and remaining gaps.
- [Funding lifecycle review, 3 November 2026](research/2026-11-03-funding-adversarial.md).
- [Historical protocol development reference](reference/protocol-development.md):
  the detailed preorganization README, retained with a dated status banner.
- [Milestone 5 specification](m5-spec.md): historical design and refutation record;
  later source changes and security notes supersede some sections.

Research documents describe the source and evidence available on their dates.
Historical test counts, third-party comparisons and old deployment notes are not
current operating attestations. Preserve their attribution when extending the work.
