# Contract Event Catalogue & Schema Specification

Events are a formal public interface. Once indexing pipelines, telemetry engines, or downstream integrations depend on them, altering event schemas without explicit versioning breaks consumers silently.

Every event emitted by `modeltrace` contracts embeds an explicit schema version in its topic tuple (`version: u32 = 1`).

## Table of Contents

- [Schema Compatibility & Evolution Policy](#schema-compatibility--evolution-policy)
- [audit-registry](#audit-registry)
- [payment-router](#payment-router)
- [usage-meter](#usage-meter)
- [Automated Catalogue Generation & Verification](#automated-catalogue-generation--verification)

---

## Schema Compatibility & Evolution Policy

### 1. Invariant: Version in Every Event Topic Tuple
Every event in the workspace defines `#[topic] pub version: u32` as its first topic field. The resulting Soroban topic vector is always ordered as:
```
[0] Event Identifier Symbol (e.g. Symbol::new(&env, "EscrowOpened"))
[1] Schema Version: u32 (current = 1)
[2..N] Contextual entity identifiers (e.g., id: u64, payer: Address, etc.)
```
Indexers can filter topic 1 to immediately route or ignore payloads by version.

### 2. Compatibility Rules
- **Additive Changes Within a Version**: Adding non-topic data fields with default/optional values or adding new independent event types is permitted within a schema version.
- **Breaking Changes Require Version Bump**: Any reordering of topics, modification of topic types, removal of fields, or changes in semantic interpretation is strictly prohibited within the current version. Such changes require incrementing `EVENT_SCHEMA_VERSION` (e.g. from `1` to `2`).
- **Dual-Emission During Migration Windows**: When a new schema version is introduced, contracts must emit both the deprecated version event and the new version event simultaneously during an announced migration window. This allows downstream consumers to migrate safely without data loss or service disruption.
- **Test Enforcement**: Shape assertion tests (`test_event_shapes_and_versioning`) run in CI to catch any unintended change at compile/test time.

---

## `audit-registry`

Attestation registry and caller authorization allowlist.

### `CallerAllowed`

- **Emitting Entrypoint**: `allow_caller(contract)`
- **Topic Tuple**: `(Symbol("CallerAllowed"), version: u32, contract: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"CallerAllowed"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `contract` | `Address` | Indexed topic parameter `contract` |

**Data Payload Layout:**

*None (empty payload / unit data)*


### `CallerRevoked`

- **Emitting Entrypoint**: `revoke_caller(contract)`
- **Topic Tuple**: `(Symbol("CallerRevoked"), version: u32, contract: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"CallerRevoked"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `contract` | `Address` | Indexed topic parameter `contract` |

**Data Payload Layout:**

*None (empty payload / unit data)*


### `AttestationRecorded`

- **Emitting Entrypoint**: `submit_attestation(caller, subject, id, model_version, policy_ref)`
- **Topic Tuple**: `(Symbol("AttestationRecorded"), version: u32, id: BytesN<32>, subject: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"AttestationRecorded"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `BytesN<32>` | Indexed topic parameter `id` |
| 3 | `subject` | `Address` | Indexed topic parameter `subject` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `submitter` | `Address` | Event data payload field |

### `AttestationSuperseded`

- **Emitting Entrypoint**: `supersede_attestation(caller, old_id, new_id)`
- **Topic Tuple**: `(Symbol("AttestationSuperseded"), version: u32, old_id: BytesN<32>)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"AttestationSuperseded"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `old_id` | `BytesN<32>` | Indexed topic parameter `old_id` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `new_id` | `BytesN<32>` | Event data payload field |

---

## `payment-router`

Escrow, incremental partial settlements, disputes, and recovery.

### `EscrowOpened`

- **Emitting Entrypoint**: `open_escrow(payer, payee, token, amount)`
- **Topic Tuple**: `(Symbol("EscrowOpened"), version: u32, id: u64, payer: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"EscrowOpened"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |
| 3 | `payer` | `Address` | Indexed topic parameter `payer` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `payee` | `Address` | Event data payload field |
| `total` | `i128` | Event data payload field |

### `PartialReleased`

- **Emitting Entrypoint**: `release_partial(id, amount)`
- **Topic Tuple**: `(Symbol("PartialReleased"), version: u32, id: u64)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"PartialReleased"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `amount` | `i128` | Event data payload field |
| `released_total` | `i128` | Event data payload field |
| `release_count` | `u32` | Event data payload field |

### `Claimed`

- **Emitting Entrypoint**: `claim(id)`
- **Topic Tuple**: `(Symbol("Claimed"), version: u32, id: u64, payee: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"Claimed"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |
| 3 | `payee` | `Address` | Indexed topic parameter `payee` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `amount` | `i128` | Event data payload field |

### `Disputed`

- **Emitting Entrypoint**: `dispute(id, amount)`
- **Topic Tuple**: `(Symbol("Disputed"), version: u32, id: u64)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"Disputed"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `amount` | `i128` | Event data payload field |

### `DisputeResolved`

- **Emitting Entrypoint**: `resolve_dispute(id, to_payee)`
- **Topic Tuple**: `(Symbol("DisputeResolved"), version: u32, id: u64)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"DisputeResolved"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `amount` | `i128` | Event data payload field |
| `to_payee` | `bool` | Event data payload field |

### `EscrowClosed`

- **Emitting Entrypoint**: `close_escrow(id)`
- **Topic Tuple**: `(Symbol("EscrowClosed"), version: u32, id: u64)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"EscrowClosed"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `refunded` | `i128` | Event data payload field |

### `SettlementFailed`

- **Emitting Entrypoint**: `report_failure(id, reason)`
- **Topic Tuple**: `(Symbol("SettlementFailed"), version: u32, id: u64, payee: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"SettlementFailed"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |
| 3 | `payee` | `Address` | Indexed topic parameter `payee` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `reason` | `FailureReason` | Event data payload field |
| `failures` | `u32` | Event data payload field |
| `exhausted` | `bool` | Event data payload field |

### `SettlementRecovered`

- **Emitting Entrypoint**: `clear_failure(id)`
- **Topic Tuple**: `(Symbol("SettlementRecovered"), version: u32, id: u64, payee: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"SettlementRecovered"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |
| 3 | `payee` | `Address` | Indexed topic parameter `payee` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `amount` | `i128` | Event data payload field |

### `DeadLettered`

- **Emitting Entrypoint**: `dead_letter(id)`
- **Topic Tuple**: `(Symbol("DeadLettered"), version: u32, id: u64, payer: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"DeadLettered"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `id` | `u64` | Indexed topic parameter `id` |
| 3 | `payer` | `Address` | Indexed topic parameter `payer` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `amount` | `i128` | Event data payload field |
| `reason` | `FailureReason` | Event data payload field |

---

## `usage-meter`

Metered billing units linked to verified attestations.

### `UsageRecorded`

- **Emitting Entrypoint**: `record_usage(payer, attestation_id, units)`
- **Topic Tuple**: `(Symbol("UsageRecorded"), version: u32, payer: Address, attested: bool)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"UsageRecorded"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `payer` | `Address` | Indexed topic parameter `payer` |
| 3 | `attested` | `bool` | Indexed topic parameter `attested` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `id` | `u64` | Event data payload field |
| `units` | `u64` | Event data payload field |
| `attestation_id` | `Option<BytesN<32>>` | Event data payload field |

### `PolicySet`

- **Emitting Entrypoint**: `set_unattested_policy(payer, policy)`
- **Topic Tuple**: `(Symbol("PolicySet"), version: u32, payer: Address)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"PolicySet"`) |
| 1 | `version` | `u32` | Schema version identifier |
| 2 | `payer` | `Address` | Indexed topic parameter `payer` |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `policy` | `UnattestedPolicy` | Event data payload field |

### `RegistrySet`

- **Emitting Entrypoint**: `set_registry(registry)`
- **Topic Tuple**: `(Symbol("RegistrySet"), version: u32)`

**Topics:**

| Index | Field | Type | Description |
|---|---|---|---|
| 0 | *event_name* | `Symbol` | Event identifier (`"RegistrySet"`) |
| 1 | `version` | `u32` | Schema version identifier |

**Data Payload Layout:**

| Field | Type | Description |
|---|---|---|
| `registry` | `Address` | Event data payload field |

---

## Automated Catalogue Generation & Verification

This catalogue is generated from Rust contract source code using `scripts/generate_event_catalogue.py`.

To regenerate or verify the documentation in CI:
```bash
python3 scripts/generate_event_catalogue.py --check
```
