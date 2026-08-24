#![no_std]
//! Metered billing units, linked to the attestations that justify them.
//!
//! A usage record here is only as trustworthy as its link back to
//! `audit-registry`. Metering with no attestation, or an attestation with no
//! metering, leaves an invoice that cannot be traced to the evidence behind it
//! — so every record carries the attestation it was priced against, verified
//! at write time rather than asserted by the caller.

use soroban_sdk::{
    contract, contractclient, contracterror, contractevent, contractimpl, contracttype,
    panic_with_error, Address, BytesN, Env, IntoVal,
};

/// Persistent entries are bumped to ~30 days, renewed once inside ~15 days.
const PERSISTENT_TTL: u32 = 518_400;
const PERSISTENT_THRESHOLD: u32 = 259_200;

/// The one method this contract needs from `audit-registry`, declared locally
/// rather than by depending on that crate. Depending on the contract crate
/// would pull its `#[contractimpl]`-generated WASM exports (`initialize`,
/// `version`, ...) into this binary and collide at link time with this
/// contract's own exports of the same names.
#[contractclient(name = "AuditRegistryClient")]
pub trait AuditRegistryInterface {
    /// True when the attestation exists, is not superseded, and belongs to
    /// `subject`. A read, so it needs no authorization of its own.
    fn verify_attestation(env: Env, id: BytesN<32>, subject: Address) -> bool;
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    /// No `audit-registry` address configured, so nothing can be verified.
    RegistryNotSet = 3,
    /// This attestation has already been metered. Usage is billed once per
    /// attestation; a second record against the same id would be double billing.
    AlreadyMetered = 4,
    /// Usage arrived without a verifiable attestation and the payer's policy is
    /// the strict default.
    AttestationRequired = 5,
    ZeroUnits = 6,
    RecordNotFound = 7,
    UnitsOverflow = 8,
}

/// What to do with usage that has no verifiable attestation behind it.
///
/// The default is `Reject` for every payer, chosen rather than inherited: an
/// unset policy is the strict one, so a payer only ever accepts unattested
/// billing by explicitly opting into it.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnattestedPolicy {
    /// Refuse the write. The default.
    Reject,
    /// Record it, flagged `attested: false`, and keep it in a separate total so
    /// it can be priced on different terms downstream.
    RecordUnattested,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Registry,
    Policy(Address),
    Record(u64),
    /// Presence marks an attestation id as already metered.
    Metered(BytesN<32>),
    Counter,
    AttestedUnits(Address),
    UnattestedUnits(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageRecord {
    pub id: u64,
    pub payer: Address,
    /// The attestation this usage was priced against. `None` only ever appears
    /// on a record whose payer opted into `RecordUnattested`.
    pub attestation_id: Option<BytesN<32>>,
    pub units: u64,
    pub attested: bool,
    pub ledger: u32,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageRecorded {
    #[topic]
    pub payer: Address,
    #[topic]
    pub attested: bool,
    pub id: u64,
    pub units: u64,
    pub attestation_id: Option<BytesN<32>>,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicySet {
    #[topic]
    pub payer: Address,
    pub policy: UnattestedPolicy,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySet {
    pub registry: Address,
}

/// Metered billing units and quotas.
#[contract]
pub struct UsageMeter;

#[contractimpl]
impl UsageMeter {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Self::bump_instance(&env);
    }

    /// Point this meter at an `audit-registry` deployment. Admin only, and
    /// changeable — the registry is an upgrade seam, not a constant.
    pub fn set_registry(env: Env, registry: Address) {
        Self::admin(&env).require_auth();
        env.storage().instance().set(&DataKey::Registry, &registry);
        Self::bump_instance(&env);
        RegistrySet { registry }.publish(&env);
    }

    /// Set a payer's policy for usage with no verifiable attestation.
    ///
    /// Authorized by the payer, not the admin: this decides whether the payer
    /// can be billed for usage nothing on-chain vouches for, so it is the
    /// payer's call to make and no one else's.
    pub fn set_unattested_policy(env: Env, payer: Address, policy: UnattestedPolicy) {
        payer.require_auth_for_args((policy,).into_val(&env));
        env.storage()
            .persistent()
            .set(&DataKey::Policy(payer.clone()), &policy);
        env.storage().persistent().extend_ttl(
            &DataKey::Policy(payer.clone()),
            PERSISTENT_THRESHOLD,
            PERSISTENT_TTL,
        );
        PolicySet { payer, policy }.publish(&env);
    }

    /// A payer's effective policy. Unset means `Reject`.
    pub fn unattested_policy(env: Env, payer: Address) -> UnattestedPolicy {
        env.storage()
            .persistent()
            .get(&DataKey::Policy(payer))
            .unwrap_or(UnattestedPolicy::Reject)
    }

    /// Record metered usage for `payer`.
    ///
    /// The payer's signature is bound to the attestation id and the unit count
    /// with `require_auth_for_args`, so an intermediate cannot take a signature
    /// given for one metering and spend it on a larger one — see the
    /// cross-contract authorization pattern this workspace follows.
    ///
    /// Verification happens here, against the registry, at write time. The
    /// caller's claim that an attestation exists is never taken at face value.
    pub fn record_usage(
        env: Env,
        payer: Address,
        attestation_id: Option<BytesN<32>>,
        units: u64,
    ) -> u64 {
        if units == 0 {
            panic_with_error!(&env, Error::ZeroUnits);
        }
        payer.require_auth_for_args((attestation_id.clone(), units).into_val(&env));

        let attested = match &attestation_id {
            Some(id) => {
                // Checked before the cross-contract call: a replay is the cheap
                // case to reject and there is no reason to pay for a verify to
                // find that out.
                if env
                    .storage()
                    .persistent()
                    .has(&DataKey::Metered(id.clone()))
                {
                    panic_with_error!(&env, Error::AlreadyMetered);
                }
                Self::verify(&env, id, &payer)
            }
            None => false,
        };

        if !attested
            && Self::unattested_policy(env.clone(), payer.clone()) == UnattestedPolicy::Reject
        {
            panic_with_error!(&env, Error::AttestationRequired);
        }

        // Only a *verified* attestation is burned. An id that failed
        // verification has metered nothing, so marking it would let a typo
        // permanently block the real attestation that shares the id.
        if attested {
            if let Some(id) = &attestation_id {
                let key = DataKey::Metered(id.clone());
                env.storage().persistent().set(&key, &());
                env.storage()
                    .persistent()
                    .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);
            }
        }

        let id = Self::next_id(&env);
        let record = UsageRecord {
            id,
            payer: payer.clone(),
            // A record that failed verification does not get to keep the
            // reference — carrying it would make an unattested record look
            // sourced when read back.
            attestation_id: if attested {
                attestation_id.clone()
            } else {
                None
            },
            units,
            attested,
            ledger: env.ledger().sequence(),
        };

        let key = DataKey::Record(id);
        env.storage().persistent().set(&key, &record);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);

        Self::add_units(&env, &payer, units, attested);
        Self::bump_instance(&env);

        UsageRecorded {
            payer,
            attested,
            id,
            units,
            attestation_id: record.attestation_id,
        }
        .publish(&env);

        id
    }

    pub fn get_usage(env: Env, id: u64) -> UsageRecord {
        match env.storage().persistent().get(&DataKey::Record(id)) {
            Some(r) => r,
            None => panic_with_error!(&env, Error::RecordNotFound),
        }
    }

    /// True once an attestation has been metered. Exposed so a submitter can
    /// check before paying for a call that would be rejected.
    pub fn is_metered(env: Env, attestation_id: BytesN<32>) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Metered(attestation_id))
    }

    /// Units backed by a verified attestation. Kept apart from the unattested
    /// total so the two can be priced on different terms.
    pub fn attested_units(env: Env, payer: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::AttestedUnits(payer))
            .unwrap_or(0)
    }

    pub fn unattested_units(env: Env, payer: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::UnattestedUnits(payer))
            .unwrap_or(0)
    }

    pub fn version(_env: Env) -> u32 {
        2
    }

    // -----------------------------------------------------------------------

    fn verify(env: &Env, id: &BytesN<32>, payer: &Address) -> bool {
        let registry: Address = match env.storage().instance().get(&DataKey::Registry) {
            Some(r) => r,
            None => panic_with_error!(env, Error::RegistryNotSet),
        };
        AuditRegistryClient::new(env, &registry).verify_attestation(id, payer)
    }

    fn admin(env: &Env) -> Address {
        match env.storage().instance().get(&DataKey::Admin) {
            Some(a) => a,
            None => panic_with_error!(env, Error::NotInitialized),
        }
    }

    fn next_id(env: &Env) -> u64 {
        let next: u64 = env.storage().instance().get(&DataKey::Counter).unwrap_or(0) + 1;
        env.storage().instance().set(&DataKey::Counter, &next);
        next
    }

    fn add_units(env: &Env, payer: &Address, units: u64, attested: bool) {
        let key = if attested {
            DataKey::AttestedUnits(payer.clone())
        } else {
            DataKey::UnattestedUnits(payer.clone())
        };
        let current: u64 = env.storage().persistent().get(&key).unwrap_or(0);
        // Metering totals must not wrap: a wrapped total silently zeroes an
        // invoice rather than failing it.
        let updated = match current.checked_add(units) {
            Some(v) => v,
            None => panic_with_error!(env, Error::UnitsOverflow),
        };
        env.storage().persistent().set(&key, &updated);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_THRESHOLD, PERSISTENT_TTL);
    }

    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_THRESHOLD, PERSISTENT_TTL);
    }
}

#[cfg(test)]
mod test;
