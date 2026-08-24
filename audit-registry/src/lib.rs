#![no_std]
//! Attestation registry.
//!
//! This crate is also the reference implementation of the workspace's
//! cross-contract authorization pattern — see
//! [`docs/cross-contract-authorization.md`](../docs/cross-contract-authorization.md).
//! `usage-meter` reads from here, and the rules below are what make that read
//! safe to build on.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, Address,
    BytesN, Env, IntoVal, Symbol,
};

/// Persistent entries are bumped to ~30 days, renewed once inside ~15 days.
/// Attestations are the audit trail, so they are persistent rather than
/// temporary; callers renew by writing, and a lapsed one can be resubmitted
/// under the same id only after it has actually been archived.
const PERSISTENT_TTL: u32 = 518_400;
const PERSISTENT_THRESHOLD: u32 = 259_200;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    /// A contract tried to act on a subject's behalf without being on the
    /// allowlist. This is the privilege-escalation guard.
    CallerNotAllowed = 3,
    AttestationExists = 4,
    AttestationNotFound = 5,
    AlreadySuperseded = 6,
    /// The replacement attestation names a different subject than the one it
    /// claims to supersede.
    SubjectMismatch = 7,
    SelfSupersede = 8,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    /// Presence of this key is the allowlist entry; the value is unused.
    AllowedCaller(Address),
    Attestation(BytesN<32>),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attestation {
    pub id: BytesN<32>,
    /// The account the attestation is *about* — the party that will be billed
    /// against it. Authorization is anchored here, never on the submitter.
    pub subject: Address,
    /// Who actually submitted it. Informational: it carries no authority.
    pub submitter: Address,
    pub model_version: Symbol,
    pub policy_ref: BytesN<32>,
    pub ledger: u32,
    pub superseded_by: Option<BytesN<32>>,
}

/// A contract was admitted to the caller allowlist.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerAllowed {
    #[topic]
    pub contract: Address,
}

/// A contract was removed from the caller allowlist.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerRevoked {
    #[topic]
    pub contract: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationRecorded {
    #[topic]
    pub id: BytesN<32>,
    #[topic]
    pub subject: Address,
    pub submitter: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationSuperseded {
    #[topic]
    pub old_id: BytesN<32>,
    pub new_id: BytesN<32>,
}

/// Stores signed inference attestations.
#[contract]
pub struct AuditRegistry;

#[contractimpl]
impl AuditRegistry {
    /// One-time initialization. `admin` is a real `Address` and signs for it,
    /// so the deployer cannot install an admin that never consented.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Self::bump_instance(&env);
    }

    // -----------------------------------------------------------------------
    // Caller allowlist
    //
    // Soroban has no `msg.sender`. A callee cannot discover which contract
    // invoked it, so "only usage-meter may call this" has to be built rather
    // than assumed: the caller passes its own address, proves it by
    // authorizing as itself, and is then checked against this list.
    // -----------------------------------------------------------------------

    /// Admit a contract to the allowlist of callers permitted to act on a
    /// subject's behalf.
    pub fn allow_caller(env: Env, contract: Address) {
        Self::admin(&env).require_auth();
        env.storage()
            .instance()
            .set(&DataKey::AllowedCaller(contract.clone()), &());
        Self::bump_instance(&env);
        CallerAllowed { contract }.publish(&env);
    }

    /// Remove a contract from the allowlist. Takes effect immediately; already
    /// written attestations are unaffected, since the authority that created
    /// them was the subject's, not the caller's.
    pub fn revoke_caller(env: Env, contract: Address) {
        Self::admin(&env).require_auth();
        env.storage()
            .instance()
            .remove(&DataKey::AllowedCaller(contract.clone()));
        Self::bump_instance(&env);
        CallerRevoked { contract }.publish(&env);
    }

    pub fn is_caller_allowed(env: Env, contract: Address) -> bool {
        env.storage()
            .instance()
            .has(&DataKey::AllowedCaller(contract))
    }

    // -----------------------------------------------------------------------
    // Attestations
    // -----------------------------------------------------------------------

    /// Record an attestation for `subject`.
    ///
    /// Two independent checks, and both matter:
    ///
    /// 1. `subject.require_auth_for_args(...)` binds the subject's signature to
    ///    *these exact arguments*. Plain `require_auth()` would let any entry
    ///    the subject signed for this contract be replayed against different
    ///    arguments by an intermediate — which is the whole escalation this
    ///    guards against.
    /// 2. When `caller` is not the subject, the caller is acting on someone
    ///    else's behalf, so it must both prove it is who it says and be on the
    ///    allowlist. `require_auth` on a contract address is satisfied
    ///    implicitly only for the *immediate invoker*, so a contract cannot
    ///    name a different, enrolled contract here and inherit its permission
    ///    — which is the failure mode that would make the allowlist worthless.
    ///
    /// Neither check subsumes the other. The allowlist alone would let an
    /// approved contract forge attestations for any subject; the subject's
    /// signature alone would let any contract at all relay it.
    pub fn submit_attestation(
        env: Env,
        caller: Address,
        subject: Address,
        id: BytesN<32>,
        model_version: Symbol,
        policy_ref: BytesN<32>,
    ) {
        subject.require_auth_for_args(
            (id.clone(), model_version.clone(), policy_ref.clone()).into_val(&env),
        );

        if caller != subject {
            caller.require_auth();
            if !env
                .storage()
                .instance()
                .has(&DataKey::AllowedCaller(caller.clone()))
            {
                panic_with_error!(&env, Error::CallerNotAllowed);
            }
        }

        let key = DataKey::Attestation(id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::AttestationExists);
        }

        let attestation = Attestation {
            id: id.clone(),
            subject: subject.clone(),
            submitter: caller.clone(),
            model_version,
            policy_ref,
            ledger: env.ledger().sequence(),
            superseded_by: None,
        };
        env.storage().persistent().set(&key, &attestation);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);
        Self::bump_instance(&env);

        AttestationRecorded {
            id,
            subject,
            submitter: caller,
        }
        .publish(&env);
    }

    /// Mark `old_id` as replaced by `new_id`.
    ///
    /// Authorized by the subject of the existing attestation, not by whoever
    /// submitted it — a submitter that has since been removed from the
    /// allowlist must not retain the power to invalidate the record.
    pub fn supersede_attestation(
        env: Env,
        caller: Address,
        old_id: BytesN<32>,
        new_id: BytesN<32>,
    ) {
        if old_id == new_id {
            panic_with_error!(&env, Error::SelfSupersede);
        }

        let mut old = Self::load(&env, &old_id);
        if old.superseded_by.is_some() {
            panic_with_error!(&env, Error::AlreadySuperseded);
        }

        let new = Self::load(&env, &new_id);
        if new.subject != old.subject {
            panic_with_error!(&env, Error::SubjectMismatch);
        }

        old.subject
            .require_auth_for_args((old_id.clone(), new_id.clone()).into_val(&env));

        if caller != old.subject {
            caller.require_auth();
            if !env
                .storage()
                .instance()
                .has(&DataKey::AllowedCaller(caller))
            {
                panic_with_error!(&env, Error::CallerNotAllowed);
            }
        }

        old.superseded_by = Some(new_id.clone());
        let key = DataKey::Attestation(old_id.clone());
        env.storage().persistent().set(&key, &old);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);

        AttestationSuperseded { old_id, new_id }.publish(&env);
    }

    // -----------------------------------------------------------------------
    // Reads
    //
    // Deliberately outside the allowlist. A read carries no authority: it
    // returns what the ledger already exposes to anyone who can decode it, so
    // gating it would buy no confidentiality and would instead couple every
    // future reader to an admin transaction. `usage-meter` depends on
    // `verify_attestation` being callable without being enrolled here.
    // -----------------------------------------------------------------------

    pub fn get_attestation(env: Env, id: BytesN<32>) -> Attestation {
        Self::load(&env, &id)
    }

    /// The predicate `usage-meter` bills against: exists, not superseded, and
    /// belongs to the claimed subject. Returns `false` rather than trapping on
    /// an unknown id so a caller can price unattested usage instead of having
    /// its whole transaction fail.
    pub fn verify_attestation(env: Env, id: BytesN<32>, subject: Address) -> bool {
        match env
            .storage()
            .persistent()
            .get::<_, Attestation>(&DataKey::Attestation(id))
        {
            Some(a) => a.superseded_by.is_none() && a.subject == subject,
            None => false,
        }
    }

    pub fn version(_env: Env) -> u32 {
        2
    }

    // -----------------------------------------------------------------------

    fn admin(env: &Env) -> Address {
        match env.storage().instance().get(&DataKey::Admin) {
            Some(a) => a,
            None => panic_with_error!(env, Error::NotInitialized),
        }
    }

    fn load(env: &Env, id: &BytesN<32>) -> Attestation {
        match env
            .storage()
            .persistent()
            .get(&DataKey::Attestation(id.clone()))
        {
            Some(a) => a,
            None => panic_with_error!(env, Error::AttestationNotFound),
        }
    }

    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_THRESHOLD, PERSISTENT_TTL);
    }
}

#[cfg(test)]
mod test;
