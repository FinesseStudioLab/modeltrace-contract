# payment-router: failed settlement and the dead-letter path

Settlement fails for reasons the contract cannot prevent and cannot see: a
destination account that no longer exists, an asset frozen or clawed back, a
trustline removed after the escrow was opened. This is what happens then.

## Pull, not push

Release and payout are already separate. `release_partial` makes funds
claimable and calls no token; the payee pulls with `claim`. Recovery keeps that
shape — the contract never retries a push against a destination that just
failed. Push-based retry burns fees on every attempt, and an attacker who can
make a destination fail can make the contract pay for the attempts.

So there is no retry mechanism here. The retry *is* `claim`, called again by
the payee once their account works.

## States

| State | Meaning | Claim |
| --- | --- | --- |
| healthy | no failures recorded | claimable |
| failed | `failures >= 3` reported | **preserved**, still claimable |
| dead-lettered | swept after the delay | ended |

`Failed` preserves the claim. It is a signal to operators, not a confiscation.

## Classification

A reverted `transfer` takes the whole transaction with it, so there is no
in-band way for the contract to observe a failure and carry on. The payee
reports it with `report_failure`, signing for their own escrow, and classifies
it as `NoDestination`, `NoTrustline`, `AssetRestricted`, or `Other`.

The classification is not decorative: the operator response differs. A removed
trustline is fixed by the payee in one transaction; a clawed-back asset is not
something the payee can do anything about. An indexer that only sees "failed"
cannot tell those apart. `SettlementFailed` carries the reason and the running
count, and sets `exhausted` on the attempt that tips the escrow into `Failed`.

`clear_failure` resets the record once the payee's account works, so a later
failure counts from zero rather than from a stale total, and a recovered escrow
stops looking broken on a dashboard.

## Dead-letter policy

Three questions have to be answered explicitly, or the path is a liability.

**When.** `MAX_SETTLEMENT_FAILURES` (3) reported failures, *and* then
`DEAD_LETTER_DELAY` (120,960 ledgers, roughly seven days) after the escrow
entered `Failed`. Three attempts distinguishes a transient problem from a
broken destination. The week exists for the payee's benefit — a removed
trustline is fixable, and value should not be swept from under someone who is
fixing it. `dead_letter_at` reports the deadline.

**Where the value goes.** Back to the payer. The payee has demonstrably been
unable to receive it across three attempts and a week. Returning it to the
party who put it in is the only destination that requires no new trust
assumption: it is not burned, and it does not accrue to the contract or to any
treasury. This is a return, not a forfeiture.

**Who can trigger it.** The arbiter. Deliberately *not* the payer, who is the
beneficiary — a payer who could both trigger the sweep and receive it would
have an incentive to grief a payee into the failed state and take the value
back. Not the payee either, for whom it does nothing.

The sweep advances the escrow's `claimed` to its `released` before transferring,
which is the same accounting a successful pull performs. That is what stops the
payee pulling value that has already been returned, and keeps the contract's
balance equal to what it still owes.

## Events

`SettlementFailed`, `SettlementRecovered`, and `DeadLettered` are emitted at
each step so a backend can alert an operator rather than discovering a stuck
escrow by inspection.

## Storage

Recovery state lives in a `DataKey::Settlement(id)` side-car rather than as
fields on `Escrow`. Recovery is the exception — most escrows never have one —
and a side-car means those escrows pay nothing for it. It uses the same
persistent TTL policy as the escrow it describes: state that outlived its
escrow would be useless, and state that expired first would silently reset the
failure count.
