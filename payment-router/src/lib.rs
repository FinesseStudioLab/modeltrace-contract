#![no_std]
//! Escrow and settlement.
//!
//! All-or-nothing release is a poor fit for continuous inference usage: a buyer
//! will not lock a month of spend up front, and a provider will not serve a
//! month before being paid. This contract settles incrementally instead — the
//! metered portion is released as it accrues, the rest stays escrowed, and the
//! payee pulls what has been released on their own schedule.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, token,
    Address, Env, IntoVal,
};

/// Persistent entries are bumped to ~30 days, renewed once inside ~15 days.
const PERSISTENT_TTL: u32 = 518_400;
const PERSISTENT_THRESHOLD: u32 = 259_200;

/// Ceiling on partial releases per escrow.
///
/// Each release is a ledger write and a slot in a bounded, permanently stored
/// counter, so an unbounded count is unbounded state growth funded by whoever
/// opened the escrow. Thirty-two is a month of daily settlement with room to
/// spare; a payee wanting finer granularity opens a second escrow rather than
/// making one escrow grow without limit.
const MAX_RELEASES: u32 = 32;

/// Failed settlement attempts before an escrow is moved to `Failed`.
///
/// Three separate attempts is enough to distinguish a transient problem from
/// a destination that is genuinely broken, without leaving a payee who fixed
/// their trustline on the second try stuck in a recovery state.
const MAX_SETTLEMENT_FAILURES: u32 = 3;

/// Ledgers a `Failed` escrow waits before the dead-letter path opens.
///
/// Roughly seven days at five seconds a ledger. The window exists for the
/// payee's benefit: a removed trustline or a frozen asset is fixable, and the
/// value should not be swept out from under someone who is fixing it.
const DEAD_LETTER_DELAY: u32 = 120_960;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    EscrowNotFound = 3,
    EscrowClosed = 4,
    InvalidAmount = 5,
    /// The release would exceed what is escrowed and not already released or
    /// frozen. This is the invariant that keeps payouts inside the deposit.
    InsufficientEscrow = 6,
    ReleaseCapReached = 7,
    NothingToClaim = 8,
    /// A recovery step was asked for on an escrow that has not accumulated
    /// enough failed settlement attempts to be in the `Failed` state.
    SettlementNotFailed = 20,
    /// The escrow is failed, but the dead-letter delay has not yet elapsed.
    /// The payee still has time to fix their account and pull.
    DeadLetterNotReady = 21,
    AlreadyDeadLettered = 22,
    NoDisputeOpen = 9,
    DisputeAlreadyOpen = 10,
    NotTheArbiter = 11,
    SamePayerAndPayee = 12,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Arbiter,
    Escrow(u64),
    Counter,
    Settlement(u64),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub id: u64,
    pub payer: Address,
    pub payee: Address,
    pub token: Address,
    /// What the payer deposited. Never changes.
    pub total: i128,
    /// Cumulative amount moved from escrowed to claimable. Monotonic.
    pub released: i128,
    /// Cumulative amount the payee has actually pulled. Monotonic, `<= released`.
    pub claimed: i128,
    /// Frozen pending dispute resolution. Taken from the unreleased remainder
    /// only, so it can never claw back a valid earlier release.
    pub disputed: i128,
    pub release_count: u32,
    pub closed: bool,
}

impl Escrow {
    /// What a further release may draw on: the deposit, less what has already
    /// been released, less anything frozen.
    fn releasable(&self) -> i128 {
        self.total - self.released - self.disputed
    }

    /// Released but not yet pulled.
    fn claimable(&self) -> i128 {
        self.released - self.claimed
    }
}

/// Why a settlement attempt did not land.
///
/// Classified rather than swallowed: the three cases have different operator
/// responses, and an indexer that only sees "it failed" cannot tell an asset
/// freeze (nothing the payee can do) from a removed trustline (which the payee
/// fixes themselves in one transaction).
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    /// The destination account no longer exists.
    NoDestination = 0,
    /// No trustline for this asset on the destination.
    NoTrustline = 1,
    /// The asset is frozen, clawed back, or otherwise not transferable.
    AssetRestricted = 2,
    /// Anything else. Kept explicit so a new failure mode is recorded rather
    /// than being forced into a category it does not belong in.
    Other = 3,
}

/// Recovery state for an escrow whose settlement is not landing.
///
/// Stored under its own key rather than as fields on `Escrow`: recovery is the
/// exception, most escrows never have one, and a side-car entry means those
/// escrows pay nothing for it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settlement {
    pub id: u64,
    /// Consecutive reported failures.
    pub failures: u32,
    pub last_reason: FailureReason,
    /// Ledger at which the escrow entered `Failed`. Only meaningful when
    /// `failed` is set — a genesis-adjacent ledger is a valid sequence, so
    /// this field is not a flag in disguise.
    pub failed_at: u32,
    /// `failures >= MAX_SETTLEMENT_FAILURES`. The claim is preserved, not lost.
    pub failed: bool,
    pub dead_lettered: bool,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowOpened {
    #[topic]
    pub id: u64,
    #[topic]
    pub payer: Address,
    pub payee: Address,
    pub total: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialReleased {
    #[topic]
    pub id: u64,
    pub amount: i128,
    pub released_total: i128,
    pub release_count: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claimed {
    #[topic]
    pub id: u64,
    #[topic]
    pub payee: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Disputed {
    #[topic]
    pub id: u64,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolved {
    #[topic]
    pub id: u64,
    pub amount: i128,
    pub to_payee: bool,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowClosed {
    #[topic]
    pub id: u64,
    pub refunded: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementFailed {
    #[topic]
    pub id: u64,
    #[topic]
    pub payee: Address,
    pub reason: FailureReason,
    pub failures: u32,
    /// True on the attempt that tipped the escrow into `Failed`.
    pub exhausted: bool,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementRecovered {
    #[topic]
    pub id: u64,
    #[topic]
    pub payee: Address,
    pub amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadLettered {
    #[topic]
    pub id: u64,
    #[topic]
    pub payer: Address,
    pub amount: i128,
    pub reason: FailureReason,
}

/// Escrow, dispute windows, payout release.
#[contract]
pub struct PaymentRouter;

#[contractimpl]
impl PaymentRouter {
    /// One-time initialization. The arbiter is the only party that can resolve
    /// a dispute, and signs for its own installation.
    pub fn initialize(env: Env, arbiter: Address) {
        if env.storage().instance().has(&DataKey::Arbiter) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        arbiter.require_auth();
        env.storage().instance().set(&DataKey::Arbiter, &arbiter);
        Self::bump_instance(&env);
    }

    /// Deposit `total` into escrow for `payee`.
    pub fn open_escrow(
        env: Env,
        payer: Address,
        payee: Address,
        token_address: Address,
        total: i128,
    ) -> u64 {
        if total <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        if payer == payee {
            panic_with_error!(&env, Error::SamePayerAndPayee);
        }
        payer.require_auth_for_args((payee.clone(), token_address.clone(), total).into_val(&env));

        let contract = env.current_contract_address();
        token::Client::new(&env, &token_address).transfer(&payer, &contract, &total);

        let id = Self::next_id(&env);
        let escrow = Escrow {
            id,
            payer: payer.clone(),
            payee: payee.clone(),
            token: token_address,
            total,
            released: 0,
            claimed: 0,
            disputed: 0,
            release_count: 0,
            closed: false,
        };
        Self::save(&env, &escrow);
        Self::bump_instance(&env);

        EscrowOpened {
            id,
            payer,
            payee,
            total,
        }
        .publish(&env);

        id
    }

    /// Settle the metered portion, leaving the remainder escrowed.
    ///
    /// Authorized by the payer against this exact escrow and amount: the payer
    /// is the party confirming that this much usage was actually metered, and
    /// binding the signature to the amount stops an intermediate from
    /// enlarging a release the payer agreed to.
    ///
    /// Nothing is pushed here. The release only makes funds claimable; the
    /// payee pulls them separately, which keeps the transfer fee on the party
    /// receiving the value and means a payee whose destination is broken
    /// cannot make settlement fail for everyone else.
    pub fn release_partial(env: Env, id: u64, amount: i128) {
        let mut escrow = Self::load(&env, id);
        Self::assert_open(&env, &escrow);

        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        escrow
            .payer
            .require_auth_for_args((id, amount).into_val(&env));

        if escrow.release_count >= MAX_RELEASES {
            panic_with_error!(&env, Error::ReleaseCapReached);
        }
        // The invariant, enforced on every path: releases can never sum past
        // the deposit, and can never dip into a frozen portion.
        if amount > escrow.releasable() {
            panic_with_error!(&env, Error::InsufficientEscrow);
        }

        escrow.released += amount;
        escrow.release_count += 1;
        Self::save(&env, &escrow);

        PartialReleased {
            id,
            amount,
            released_total: escrow.released,
            release_count: escrow.release_count,
        }
        .publish(&env);
    }

    /// Pull everything released and not yet taken.
    ///
    /// Callable after the escrow is closed, so a payee is never stranded by a
    /// refund of the remainder.
    pub fn claim(env: Env, id: u64) -> i128 {
        let mut escrow = Self::load(&env, id);
        escrow.payee.require_auth();

        let amount = escrow.claimable();
        if amount <= 0 {
            panic_with_error!(&env, Error::NothingToClaim);
        }

        escrow.claimed += amount;
        Self::save(&env, &escrow);

        token::Client::new(&env, &escrow.token).transfer(
            &env.current_contract_address(),
            &escrow.payee,
            &amount,
        );

        Claimed {
            id,
            payee: escrow.payee,
            amount,
        }
        .publish(&env);

        amount
    }

    /// Freeze part of the unreleased remainder pending arbitration.
    ///
    /// Deliberately cannot touch `released`. Funds released before the dispute
    /// were validly settled against usage that was already metered, and a
    /// later disagreement about subsequent usage is not a reason to claw them
    /// back — that is the difference between disputing a portion and freezing
    /// the whole relationship.
    pub fn dispute(env: Env, id: u64, amount: i128) {
        let mut escrow = Self::load(&env, id);
        Self::assert_open(&env, &escrow);

        if escrow.disputed > 0 {
            panic_with_error!(&env, Error::DisputeAlreadyOpen);
        }
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        escrow
            .payer
            .require_auth_for_args((id, amount).into_val(&env));

        if amount > escrow.releasable() {
            panic_with_error!(&env, Error::InsufficientEscrow);
        }

        escrow.disputed = amount;
        Self::save(&env, &escrow);

        Disputed { id, amount }.publish(&env);
    }

    /// Resolve an open dispute. Arbiter only.
    ///
    /// In the payee's favour the frozen amount becomes claimable; in the
    /// payer's favour it returns to the releasable remainder rather than being
    /// refunded outright, so a resolved dispute leaves the escrow usable
    /// instead of forcing it to be torn down and reopened.
    ///
    /// Resolution does not consume a partial-release slot: the cap exists to
    /// bound how often the payer can voluntarily settle, and an arbitrated
    /// outcome is not that.
    pub fn resolve_dispute(env: Env, id: u64, to_payee: bool) {
        Self::arbiter(&env).require_auth();

        let mut escrow = Self::load(&env, id);
        if escrow.disputed <= 0 {
            panic_with_error!(&env, Error::NoDisputeOpen);
        }

        let amount = escrow.disputed;
        escrow.disputed = 0;
        if to_payee {
            escrow.released += amount;
        }
        Self::save(&env, &escrow);

        DisputeResolved {
            id,
            amount,
            to_payee,
        }
        .publish(&env);
    }

    /// Close the escrow and refund whatever is still unreleased to the payer.
    ///
    /// Refuses while a dispute is open — closing then would refund the frozen
    /// amount out from under the arbitration. Anything already released stays
    /// claimable by the payee afterwards.
    pub fn close_escrow(env: Env, id: u64) {
        let mut escrow = Self::load(&env, id);
        Self::assert_open(&env, &escrow);

        if escrow.disputed > 0 {
            panic_with_error!(&env, Error::DisputeAlreadyOpen);
        }
        escrow.payer.require_auth_for_args((id,).into_val(&env));

        let refund = escrow.releasable();
        escrow.closed = true;
        Self::save(&env, &escrow);

        if refund > 0 {
            token::Client::new(&env, &escrow.token).transfer(
                &env.current_contract_address(),
                &escrow.payer,
                &refund,
            );
        }

        EscrowClosed {
            id,
            refunded: refund,
        }
        .publish(&env);
    }

    // -----------------------------------------------------------------------
    // Failed settlement and recovery
    // -----------------------------------------------------------------------

    /// Record a settlement attempt that did not land.
    ///
    /// The contract cannot observe why a pull failed — a reverted `transfer`
    /// takes the whole transaction with it, so there is no in-band way to
    /// classify and continue. The payee reports it instead, signing for their
    /// own escrow, and the classification is what the backend alerts on.
    ///
    /// Nothing is pushed and nothing is retried here. The claim is preserved
    /// exactly as it was; this only records that the destination is not
    /// working, which is what eventually opens the dead-letter path.
    pub fn report_failure(env: Env, id: u64, reason: FailureReason) -> u32 {
        let escrow = Self::load(&env, id);
        escrow.payee.require_auth_for_args((id,).into_val(&env));

        let mut state = Self::settlement(&env, id);
        // Checked before the claim amount: after a sweep the claim is legitimately
        // zero, and reporting NothingToClaim there would name a symptom of the
        // dead-lettering rather than the dead-lettering itself.
        if state.dead_lettered {
            panic_with_error!(&env, Error::AlreadyDeadLettered);
        }
        if escrow.claimable() <= 0 {
            panic_with_error!(&env, Error::NothingToClaim);
        }

        state.failures += 1;
        state.last_reason = reason;

        let exhausted = !state.failed && state.failures >= MAX_SETTLEMENT_FAILURES;
        if exhausted {
            state.failed = true;
            state.failed_at = env.ledger().sequence();
        }
        Self::save_settlement(&env, &state);

        SettlementFailed {
            id,
            payee: escrow.payee,
            reason,
            failures: state.failures,
            exhausted,
        }
        .publish(&env);

        state.failures
    }

    /// Clear the failure record after the payee has fixed their account.
    ///
    /// The recovery path is the ordinary `claim` — the payee pulls once their
    /// trustline is back. This exists so a recovered escrow stops looking
    /// broken to an operator, and so a later failure counts from zero rather
    /// than from a stale total.
    pub fn clear_failure(env: Env, id: u64) {
        let escrow = Self::load(&env, id);
        escrow.payee.require_auth_for_args((id,).into_val(&env));

        let state = Self::settlement(&env, id);
        if !state.failed {
            panic_with_error!(&env, Error::SettlementNotFailed);
        }
        if state.dead_lettered {
            panic_with_error!(&env, Error::AlreadyDeadLettered);
        }

        let amount = escrow.claimable();
        Self::save_settlement(
            &env,
            &Settlement {
                id,
                failures: 0,
                last_reason: FailureReason::Other,
                failed_at: 0,
                failed: false,
                dead_lettered: false,
            },
        );

        SettlementRecovered {
            id,
            payee: escrow.payee,
            amount,
        }
        .publish(&env);
    }

    /// Dead-letter a settlement that has stayed broken.
    ///
    /// **Policy:** the value returns to the payer. The payee has demonstrably
    /// been unable to receive it across `MAX_SETTLEMENT_FAILURES` attempts and
    /// a `DEAD_LETTER_DELAY` window, and returning it to the party that put it
    /// in is the only destination that needs no new trust assumption. It is
    /// not burned and it does not accrue to the contract.
    ///
    /// **Who may trigger it:** the arbiter. Not the payer, who would otherwise
    /// have an incentive to grief a payee into the failed state and sweep;
    /// not the payee, for whom it does nothing.
    ///
    /// The escrow's claimed total is advanced to its released total, so the
    /// swept amount stops being claimable. This is the same accounting a
    /// successful pull performs, which keeps the balance invariant intact.
    pub fn dead_letter(env: Env, id: u64) -> i128 {
        Self::arbiter(&env).require_auth();

        let mut escrow = Self::load(&env, id);
        let mut state = Self::settlement(&env, id);

        if !state.failed {
            panic_with_error!(&env, Error::SettlementNotFailed);
        }
        if state.dead_lettered {
            panic_with_error!(&env, Error::AlreadyDeadLettered);
        }
        if env.ledger().sequence() < state.failed_at + DEAD_LETTER_DELAY {
            panic_with_error!(&env, Error::DeadLetterNotReady);
        }

        let amount = escrow.claimable();
        if amount <= 0 {
            panic_with_error!(&env, Error::NothingToClaim);
        }

        // State first, token second. Advancing `claimed` here is what stops
        // the payee pulling the same value after it has been swept.
        escrow.claimed += amount;
        state.dead_lettered = true;
        Self::save(&env, &escrow);
        Self::save_settlement(&env, &state);

        token::Client::new(&env, &escrow.token).transfer(
            &env.current_contract_address(),
            &escrow.payer,
            &amount,
        );

        DeadLettered {
            id,
            payer: escrow.payer,
            amount,
            reason: state.last_reason,
        }
        .publish(&env);

        amount
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    pub fn get_escrow(env: Env, id: u64) -> Escrow {
        Self::load(&env, id)
    }

    /// Released but not yet pulled by the payee.
    pub fn claimable(env: Env, id: u64) -> i128 {
        Self::load(&env, id).claimable()
    }

    /// What a further release may still draw on.
    pub fn releasable(env: Env, id: u64) -> i128 {
        Self::load(&env, id).releasable()
    }

    /// Recovery state for an escrow. Healthy escrows report zeroed counters
    /// rather than an error, so a caller need not special-case the common path.
    pub fn settlement_status(env: Env, id: u64) -> Settlement {
        Self::load(&env, id);
        Self::settlement(&env, id)
    }

    /// Ledger at which the dead-letter path opens, or `None` while the escrow
    /// is healthy.
    pub fn dead_letter_at(env: Env, id: u64) -> Option<u32> {
        let state = Self::settlement(&env, id);
        if state.failed && !state.dead_lettered {
            Some(state.failed_at + DEAD_LETTER_DELAY)
        } else {
            None
        }
    }

    pub fn max_settlement_failures(_env: Env) -> u32 {
        MAX_SETTLEMENT_FAILURES
    }

    pub fn max_releases(_env: Env) -> u32 {
        MAX_RELEASES
    }

    pub fn version(_env: Env) -> u32 {
        2
    }

    // -----------------------------------------------------------------------

    fn arbiter(env: &Env) -> Address {
        match env.storage().instance().get(&DataKey::Arbiter) {
            Some(a) => a,
            None => panic_with_error!(env, Error::NotInitialized),
        }
    }

    fn load(env: &Env, id: u64) -> Escrow {
        match env.storage().persistent().get(&DataKey::Escrow(id)) {
            Some(e) => e,
            None => panic_with_error!(env, Error::EscrowNotFound),
        }
    }

    /// Recovery state, defaulted for an escrow that has never failed.
    fn settlement(env: &Env, id: u64) -> Settlement {
        env.storage()
            .persistent()
            .get(&DataKey::Settlement(id))
            .unwrap_or(Settlement {
                id,
                failures: 0,
                last_reason: FailureReason::Other,
                failed_at: 0,
                failed: false,
                dead_lettered: false,
            })
    }

    /// Same TTL policy as the escrow itself: recovery state that outlived the
    /// escrow it describes would be useless, and one that expired first would
    /// silently reset the failure count.
    fn save_settlement(env: &Env, state: &Settlement) {
        let key = DataKey::Settlement(state.id);
        env.storage().persistent().set(&key, state);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);
    }

    fn save(env: &Env, escrow: &Escrow) {
        let key = DataKey::Escrow(escrow.id);
        env.storage().persistent().set(&key, escrow);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);
    }

    fn assert_open(env: &Env, escrow: &Escrow) {
        if escrow.closed {
            panic_with_error!(env, Error::EscrowClosed);
        }
    }

    fn next_id(env: &Env) -> u64 {
        let next: u64 = env.storage().instance().get(&DataKey::Counter).unwrap_or(0) + 1;
        env.storage().instance().set(&DataKey::Counter, &next);
        next
    }

    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_THRESHOLD, PERSISTENT_TTL);
    }
}

#[cfg(test)]
mod test;
