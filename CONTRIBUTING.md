# Contributing to modeltrace-contract

These are the Soroban contracts that hold ModelTrace's rules: attestation
(`audit-registry`), metering (`usage-meter`), and settlement (`payment-router`).
Contract code is the part of the system that is hardest to change after
deployment, so the review bar here is deliberately higher than for the apps.

## Prerequisites

- Rust stable (1.84+ — the `wasm32v1-none` target requires it)
- The WASM target: `rustup target add wasm32v1-none`

> `wasm32-unknown-unknown` does **not** work with the current Soroban SDK: Rust
> 1.82+ enables reference-types and multi-value on that target, which the Soroban
> environment does not support. Always build against `wasm32v1-none`.

## Local workflow

```bash
cargo fmt --all                                  # format
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                           # tests
cargo build --release --target wasm32v1-none     # deployable artifacts
```

All four must pass before you open a PR — CI runs exactly these.

## Review bar

- **Authorization is explicit.** Anything that moves funds, changes an admin, or
  writes reputation calls `require_auth` on a concrete `Address`. A PR that
  touches state without an auth story will be sent back.
- **Arithmetic is checked.** No silent wrapping in metering or settlement. State
  the rounding direction and who it favours.
- **Storage has a TTL story.** Say which storage type you used (instance,
  persistent, temporary) and why, and how entries get extended or expire.
- **Events for anything observable off-chain.** Indexers and the audit export
  path depend on them.
- **Tests come with the change.** Use `soroban_sdk::testutils`; cover the failure
  paths, not just the happy one.
- **Interface changes need a migration note** in the PR description. Deployed
  contracts have integrators.

## Commits and PRs

Conventional Commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`). Keep PRs
scoped to one concern. Open a draft early for anything architectural — it is much
cheaper to redirect a design than to review it at the end.
