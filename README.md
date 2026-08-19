# ModelTrace — Soroban contracts

> **Verifiable AI inference accounting, billing, and compliance on Stellar.**

This repository holds the on-chain rules for ModelTrace. The contracts are the
source of truth: they decide what counts as an attested inference, how usage is
metered, and when money moves. Everything else — the
[API](https://github.com/FinesseStudioLab/modeltrace-backend) and the
[web app](https://github.com/FinesseStudioLab/modeltrace-frontend) — is an
interface onto what is written here.

---

## Why this exists

AI procurement is scaling faster than AI governance. Three problems recur:

- Teams cannot consistently prove **which model version**, in **which region**,
  under **which policy** produced a given output.
- Regulated buyers need audit trails that **survive vendor churn** and outlive a
  spreadsheet export.
- Usage-based inference billing has **no shared neutral layer**, so disputes
  between buyer and provider come down to whose dashboard you believe.

ModelTrace puts attestation, metering, and settlement on a ledger both parties
can read and neither party controls.

---

## The three contracts

| Crate | Role | Holds |
| --- | --- | --- |
| [`audit-registry`](audit-registry/) | Attestation | Signed inference events — model version, policy ref, timestamp, submitter |
| [`usage-meter`](usage-meter/) | Metering | Usage units, quota buckets, pricing tiers |
| [`payment-router`](payment-router/) | Settlement | Escrow, dispute windows, payout release |

They are deliberately separate so each can be reasoned about — and audited — on
its own. Attestation must be cheap and frequent; settlement must be conservative
and rare.

### Current status

**These are compiling scaffolds, not production contracts.** Each crate today
exposes `initialize`, `ping`, and `version` and nothing more. Before any of this
is trustworthy it needs, at minimum:

1. Real domain entrypoints and storage maps in place of `ping`
2. `require_auth` on every path that touches funds, roles, or reputation
3. `Address`-based identity instead of the current `Symbol` admin placeholder
4. Events on every state change, so indexers and audit exports have a source
5. Test coverage on failure paths, not just happy paths

The open issues on this repository track that work.

---

## Build

Requires Rust 1.84+ and the Soroban WASM target:

```bash
rustup target add wasm32v1-none
```

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --target wasm32v1-none
```

Deployable artifacts land in `target/wasm32v1-none/release/*.wasm`.

> **Use `wasm32v1-none`, not `wasm32-unknown-unknown`.** Rust 1.82+ enables
> reference-types and multi-value on the latter, which the Soroban environment
> rejects. The SDK will fail the build with an explicit message if you try.

---

## Layout

```
├── Cargo.toml            # workspace: members, shared deps, release profile
├── audit-registry/       # attestation
├── usage-meter/          # metering and quotas
├── payment-router/       # escrow and settlement
└── .github/workflows/    # fmt, clippy, test, WASM build
```

---

## Contributing

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) — the review bar for contract changes is
higher than for the apps, and it explains why. Security reports go through
[`SECURITY.md`](SECURITY.md), privately, never as a public issue.

## License

[Apache-2.0](LICENSE)
