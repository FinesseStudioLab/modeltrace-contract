# Cross-contract authorization

`usage-meter` will call `audit-registry`, and `payment-router` will call
`usage-meter`. This document fixes the rules for those calls before they are
wired, so that "contract A may do X on behalf of user U" is something the code
enforces rather than something the call graph implies.

`audit-registry` is the reference implementation. New cross-contract edges
should follow the same shape.

---

## The failure this prevents

Soroban has no `msg.sender`. A contract cannot discover who invoked it, so a
callee that wants to restrict its callers has to be *told* who is calling and
then verify the claim. The tempting shortcut — trust the address passed in, or
trust that only the intended contract knows the entrypoint exists — produces a
contract that looks permission-checked and is not.

The second, subtler failure is authority laundering. A user signs one
authorization for a transaction; an intermediate contract then spends that
authorization on a different action than the user agreed to. Plain
`require_auth()` is vulnerable to this whenever an intermediate chooses the
arguments.

Both are privilege escalation, and neither shows up under `mock_all_auths`.

---

## The pattern

Every entrypoint reachable by another contract takes an explicit
`caller: Address` and applies three checks in this order:

| # | Check | Guards against |
|---|---|---|
| 1 | `subject.require_auth_for_args(...)` over the arguments that determine the effect | An intermediate spending the user's signature on different arguments |
| 2 | `caller.require_auth()` when `caller != subject` | A contract naming a different contract as the caller |
| 3 | `caller` present in the admin-managed allowlist | An arbitrary contract relaying for users at all |

None of the three is redundant:

- Without **1**, an approved intermediate could take a signature the subject
  gave for one attestation and submit a different one.
- Without **2**, any contract could pass an allowlisted contract's address and
  inherit its standing. This one is load-bearing and non-obvious: a contract
  address satisfies `require_auth` implicitly *only for the immediate invoker*,
  which is exactly why claiming someone else's address fails.
- Without **3**, the subject's signature alone would let any contract on the
  network act as a relay.

### Why the authority is anchored on the subject

The subject is the party that gets billed against the record. The submitter is
recorded but carries no authority: removing a contract from the allowlist must
not strip anyone of records already validly created, and a former submitter must
not retain the power to invalidate them. `supersede_attestation` is therefore
authorized by the subject of the existing attestation, never by whoever
originally submitted it.

### Reads are deliberately not gated

`get_attestation` and `verify_attestation` are callable by anyone. A read
carries no authority and returns what the ledger already exposes to anyone able
to decode it, so gating it would buy no confidentiality while coupling every
future reader to an admin transaction. `usage-meter` depends on
`verify_attestation` being callable without enrolment.

The allowlist covers **writes on behalf of a subject**, and nothing else.

---

## Authorization tree per flow

### Flow 1 — user submits directly

```
tx root: audit-registry.submit_attestation(caller=U, subject=U, id, model, policy)
└── auth: U  →  require_auth_for_args([id, model, policy])
```

`caller == subject`, so checks 2 and 3 do not apply. One signature, scoped to
the three arguments that define the record.

### Flow 2 — user goes through an enrolled intermediate

```
tx root: usage-meter.record_usage(...)
└── sub-invocation: audit-registry.submit_attestation(caller=M, subject=U, ...)
    ├── auth: U  →  require_auth_for_args([id, model, policy])     (signed by the user)
    └── auth: M  →  require_auth()                                  (implicit: M is the invoker)
        └── allowlist: M enrolled by admin
```

The user's entry is rooted at the `audit-registry` invocation, not at
`usage-meter`'s — the user authorizes the effect, not the route to it. An
intermediate that changes any of `id`, `model` or `policy` between receiving the
call and making it invalidates the entry.

### Flow 3 — payment-router → usage-meter

Not yet wired. When it is, it takes the same shape: `usage-meter` gains an
allowlist, `payment-router` is enrolled in it, and the party whose funds move is
the subject whose `require_auth_for_args` is checked.

---

## `require_auth` vs `require_auth_for_args`

Use `require_auth_for_args` wherever an intermediate contract chooses or can
alter the arguments — which is every entrypoint reachable cross-contract. The
argument list passed to it should be the values that determine the effect, not
the full parameter list: including `caller` would make the subject's signature
depend on which relay was used, so re-signing would be needed for a routing
change that does not alter what the subject agreed to.

Plain `require_auth` is enough for:

- admin operations, where the admin is invoking directly and the identity being
  checked is the sole authority for the action
- proving a `caller` is the immediate invoker, where the address itself is the
  entire claim

---

## Testing rules

**Any test that is about authorization must not call `mock_all_auths`.** It
approves every `require_auth` in the transaction, so it turns every one of these
checks green regardless of what the contract does. `audit-registry/src/test.rs`
uses `mock_auths` throughout, granting one signature at a time, scoped to a
single address, entrypoint and argument list.

The escalation attempts that must stay covered when this pattern is extended:

1. A signature for one argument set replayed against another
2. A third party submitting for a subject that never signed
3. An unenrolled contract relaying a genuinely signed submission
4. An enrolled contract forging for a subject that never signed
5. An unenrolled contract naming an enrolled contract as `caller`
6. Revocation taking effect immediately
7. A submitter attempting to invalidate a record it does not own

---

## Review record

| Date | Reviewer | Scope | Outcome |
|---|---|---|---|
| — | *pending* | Pattern as described above, and its implementation in `audit-registry` | *open* |

The acceptance criterion "pattern peer-reviewed and the review recorded"
explicitly requires review by someone who did not design it, so it cannot be
closed from inside this change. The table is here to be filled in by the
reviewer on this PR; please add the row rather than approving silently.
