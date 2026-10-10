# Event Catalogue and Schema Versioning

Events emitted by smart contracts form a permanent, public interface. Once indexers, data export pipelines, analytics backends, and ecosystem partners subscribe to contract events, modifying topics or reordering payload fields breaks downstream consumers silently — without the compile-time guarantees of contract entrypoint signatures.

This document establishes the schema versioning architecture, the full event catalogue, the compatibility invariants, and verification test standards across `audit-registry`, `usage-meter`, and `payment-router`.

---

## Schema Versioning Architecture

Every event in the ModelTrace contract suite embeds an explicit schema version in its topic tuple:

```rust
pub const EVENT_SCHEMA_VERSION: u32 = 1;
```

### Topic Tuple Layout Convention

Soroban events consist of up to 4 topics and a data payload. By standard convention:

| Topic Index | Field | Type | Description |
|---|---|---|---|
| `topic[0]` | Event Identifier | `Symbol` | The contract event name (e.g., `symbol!("caller_allowed")`) |
| `topic[1]` | Schema Version | `u32` | Canonical schema version (`EVENT_SCHEMA_VERSION = 1`) |
| `topic[2]` | Primary Entity Key | `Address` / `u64` / `BytesN<32>` | Indexed lookup key (e.g. `contract`, `id`, `payer`) |
| `topic[3]` | Secondary Key (Optional) | `Address` / `bool` | Filter key (e.g. `subject`, `payee`, `attested`) |

Embedding `version` as `topic[1]` provides off-chain indexers with constant-time filtering by schema version before parsing variable topic lengths or event data payloads.

---

## Event Catalogue

### 1. `audit-registry`

Stores verifiable inference attestations and maintains caller allowlists.

| Event | Topics Tuple | Data Payload | Emitted By | Description |
|---|---|---|---|---|
| `CallerAllowed` | `(Symbol("caller_allowed"), version: u32, contract: Address)` | Empty map | `allow_caller` | Emitted when an intermediate contract is admitted to the caller allowlist. |
| `CallerRevoked` | `(Symbol("caller_revoked"), version: u32, contract: Address)` | Empty map | `revoke_caller` | Emitted when a contract is revoked from the allowlist. |
| `AttestationRecorded` | `(Symbol("attestation_recorded"), version: u32, id: BytesN<32>, subject: Address)` | `submitter: Address` | `submit_attestation` | Emitted when an authorized inference attestation is stored. |
| `AttestationSuperseded` | `(Symbol("attestation_superseded"), version: u32, old_id: BytesN<32>)` | `new_id: BytesN<32>` | `supersede_attestation` | Emitted when the subject updates an attestation reference. |

---

### 2. `usage-meter`

Tracks inference usage units, billing quotas, and links to attestation records.

| Event | Topics Tuple | Data Payload | Emitted By | Description |
|---|---|---|---|---|
| `UsageRecorded` | `(Symbol("usage_recorded"), version: u32, payer: Address, attested: bool)` | `(id: u64, units: u64, attestation_id: Option<BytesN<32>>)` | `record_usage` | Emitted when metered compute usage is committed on-chain. |
| `PolicySet` | `(Symbol("policy_set"), version: u32, payer: Address)` | `policy: UnattestedPolicy` | `set_unattested_policy` | Emitted when a payer configures their unattested usage acceptance policy. |
| `RegistrySet` | `(Symbol("registry_set"), version: u32)` | `registry: Address` | `set_registry` | Emitted by admin when updating the `audit-registry` deployment address. |

---

### 3. `payment-router`

Manages incremental escrow settlement, arbitration, and failure recovery.

| Event | Topics Tuple | Data Payload | Emitted By | Description |
|---|---|---|---|---|
| `EscrowOpened` | `(Symbol("escrow_opened"), version: u32, id: u64, payer: Address)` | `(payee: Address, total: i128)` | `open_escrow` | Emitted when an escrow is funded and opened. |
| `PartialReleased` | `(Symbol("partial_released"), version: u32, id: u64)` | `(amount: i128, released_total: i128, release_count: u32)` | `release_partial` | Emitted when the payer releases a metered tranche for withdrawal. |
| `Claimed` | `(Symbol("claimed"), version: u32, id: u64, payee: Address)` | `amount: i128` | `claim` | Emitted when the payee draws released funds. |
| `Disputed` | `(Symbol("disputed"), version: u32, id: u64)` | `amount: i128` | `dispute` | Emitted when unreleased remainder is frozen pending arbitration. |
| `DisputeResolved` | `(Symbol("dispute_resolved"), version: u32, id: u64)` | `(amount: i128, to_payee: bool)` | `resolve_dispute` | Emitted when the arbiter settles the frozen amount. |
| `EscrowClosed` | `(Symbol("escrow_closed"), version: u32, id: u64)` | `refunded: i128` | `close_escrow` | Emitted when the escrow closes and refunds remainder to payer. |
| `SettlementFailed` | `(Symbol("settlement_failed"), version: u32, id: u64, payee: Address)` | `(reason: FailureReason, failures: u32, exhausted: bool)` | `report_failure` | Emitted when payee records a failed pull attempt. |
| `SettlementRecovered` | `(Symbol("settlement_recovered"), version: u32, id: u64, payee: Address)` | `amount: i128` | `clear_failure` | Emitted when payee clears failure counter after restoring account. |
| `DeadLettered` | `(Symbol("dead_lettered"), version: u32, id: u64, payer: Address)` | `(amount: i128, reason: FailureReason)` | `dead_letter` | Emitted when unrecoverable escrow is swept back to payer. |

---

## Schema Evolution and Compatibility Policy

1. **Additive Changes Within a Version**:
   - Adding new event types does not increment `EVENT_SCHEMA_VERSION`.
   - Topic order and existing topic positions are strictly immutable within a schema version.
2. **Breaking Changes**:
   - Reordering topics, changing field types, removing topics, or altering payload representations constitutes a breaking change.
   - Any breaking change requires incrementing `EVENT_SCHEMA_VERSION`.
3. **Migration Window Guarantee**:
   - If contract upgrades introduce `version = N + 1`, deprecation notices must precede the upgrade. Where feasible during upgrade cutovers, dual emission or versioned handlers allow indexers to transition without data loss.

---

## Automated Verification

Event structure invariants are formally asserted in regression test suites:
- `audit_registry::test::test_event_schema_version_and_shape`
- `usage_meter::test::test_usage_meter_event_schema_version_and_shape`
- `payment_router::test::test_payment_router_event_schema_version_and_shape`

Any inadvertent modification to event names, topic orders, or schema version triggers immediate compile and test failures in CI.
