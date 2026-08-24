# payment-router: value safety

The invariants the settlement paths hold, why each one holds, and what an
untrusted token contract can and cannot do to them. Written to be read before
the implementation, and asserted directly in `payment-router/src/invariants.rs`.

## The invariants

1. **Conservation.** The router's token balance equals the sum of what every
   escrow still owes: released-but-unpulled, plus unreleased remainder, plus
   anything frozen. No path creates or destroys value.
2. **An escrow never pays out more than it took in.** `claimed <= released` and
   `released + disputed <= total`, on every path, including after a dispute is
   resolved and after the escrow is closed.
3. **Settled paths are idempotent.** A second `close_escrow`, or a `claim`
   against a settled escrow with nothing left to pull, returns `AlreadySettled`
   and moves no funds.

## Checks-effects-interactions

Every path that moves value validates first, commits the state transition
second, and calls the token last.

| Path | Effect committed | Then |
| --- | --- | --- |
| `claim` | `claimed += amount` | `transfer` out |
| `close_escrow` | `closed = true` | refund `transfer` |
| `open_escrow` | escrow row written | deposit `transfer` in |
| `release_partial` | `released += amount` | no token call at all |

`release_partial` is the important one: it makes funds *claimable* and calls no
token. The payee pulls separately. A push-based release would put an untrusted
destination on the settlement path and let a broken payee fail everyone's
settlement; pull keeps that blast radius to the payee alone.

`open_escrow` is the one place where ordering alone is not sufficient. Writing
the escrow row before the deposit lands means that, for the duration of the
token call, the ledger records a claim on funds that have not arrived. A
hostile token could call back into `close_escrow` on that row and be refunded
its `total` out of *other escrows'* deposits. That is why the deposit also runs
inside the reentrancy guard, and why the guard is not merely belt-and-braces.

## The reentrancy guard

`enter`/`leave` take a marker in **temporary** storage under the `in_fligh`
symbol, held for the duration of a value-moving call. A second entry while one
is in flight returns `ReentrantCall`.

Temporary storage is the right class here: the marker is meaningful only within
the transaction that sets it, so the host drops it at the end of the ledger and
nobody pays rent on it. It is kept out of `DataKey` for the same reason — it is
not escrow state. On a panic the transaction reverts, which unwinds the marker
along with everything else, so a failed call cannot leave the contract wedged.

## The adversarial token

The payer chooses the token address when opening an escrow. The router must
therefore treat the token as hostile code that runs on every `transfer`. The
router's entire dependency on the token interface is `transfer`, which keeps
the surface small.

What a hostile token can do:

- **Fail the transfer.** Its own settlement reverts. No other escrow is
  affected, because each escrow names its own token.
- **Call back during `transfer`.** It gets `ReentrantCall`, which fails the
  outer call too. The reverted transaction takes the state write with it, so
  the escrow is left exactly as it was.
- **Lie about balances.** It can report anything; the router never reads the
  token's balance, only its own accounting, so a lie changes nothing.

What it cannot do:

- **Pull twice.** `claimed` is advanced before the transfer, so a re-entrant
  `claim` finds nothing claimable even if the guard were absent.
- **Reach another escrow's funds.** Covered by
  `test_a_token_that_calls_back_during_a_deposit_cannot_drain_another_escrow`.
- **Strand a third party.** Its damage is confined to escrows that named it.

Both callback cases are exercised against a real re-entering token contract in
`invariants.rs` rather than argued for in prose.
