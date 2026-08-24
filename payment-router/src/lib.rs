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
