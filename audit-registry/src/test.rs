#![cfg(test)]
//! Authorization tests for the cross-contract pattern.
//!
//! **Nothing in this file calls `mock_all_auths`.** These tests exist to prove
//! that an unauthorized intermediate contract cannot act on a user's behalf,
//! and `mock_all_auths` approves every `require_auth` in the transaction — it
//! would make every one of them pass regardless of what the contract does.
//! Every signature here is granted deliberately, with `mock_auths`, scoped to
//! one address, one entrypoint and one exact argument list.

use super::*;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    vec, Env, IntoVal,
};

// ---------------------------------------------------------------------------
// Two stand-in intermediates, mirroring how `usage-meter` will call this
// contract — one that plays by the rules, one that does not.
// ---------------------------------------------------------------------------

/// Behaves the way an approved intermediate is supposed to: it authorizes as
/// itself for exactly the sub-invocation it is about to make, and it passes
/// its own address as `caller` rather than pretending to be the subject.
#[contract]
pub struct HonestRelay;

#[contractimpl]
impl HonestRelay {
    pub fn relay(
        env: Env,
        registry: Address,
        subject: Address,
        id: BytesN<32>,
        model_version: Symbol,
        policy_ref: BytesN<32>,
    ) {
        let me = env.current_contract_address();
        env.authorize_as_current_contract(vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: registry.clone(),
                    fn_name: Symbol::new(&env, "submit_attestation"),
                    args: (
                        me.clone(),
                        subject.clone(),
                        id.clone(),
                        model_version.clone(),
                        policy_ref.clone(),
                    )
                        .into_val(&env),
                },
                sub_invocations: vec![&env],
            }),
        ]);

        AuditRegistryClient::new(&env, &registry).submit_attestation(
            &me,
            &subject,
            &id,
            &model_version,
            &policy_ref,
        );
    }

    /// Passes an address that is *not* this contract as `caller` — the shape
    /// an impersonation attempt takes, where an unenrolled contract borrows an
    /// enrolled one's standing by simply naming it.
    pub fn relay_claiming(
        env: Env,
        registry: Address,
        claimed_caller: Address,
        subject: Address,
        id: BytesN<32>,
        model_version: Symbol,
        policy_ref: BytesN<32>,
    ) {
        AuditRegistryClient::new(&env, &registry).submit_attestation(
            &claimed_caller,
            &subject,
            &id,
            &model_version,
            &policy_ref,
        );
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    env: Env,
    registry: Address,
    admin: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    let registry = env.register(AuditRegistry, ());
    let admin = Address::generate(&env);

    // Scoped to this one call: the admin signs for initialize and nothing else.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &registry,
            fn_name: "initialize",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    AuditRegistryClient::new(&env, &registry).initialize(&admin);

    Fixture {
        env,
        registry,
        admin,
    }
}

impl Fixture {
    fn client(&self) -> AuditRegistryClient<'_> {
        AuditRegistryClient::new(&self.env, &self.registry)
    }

    fn allow(&self, contract: &Address) {
        self.env.mock_auths(&[MockAuth {
            address: &self.admin,
            invoke: &MockAuthInvoke {
                contract: &self.registry,
                fn_name: "allow_caller",
                args: (contract.clone(),).into_val(&self.env),
                sub_invokes: &[],
            },
        }]);
        self.client().allow_caller(contract);
    }

    /// The subject's signature over the exact arguments `submit_attestation`
    /// binds it to — id, model version and policy ref, and nothing else.
    fn sign_submit(&self, subject: &Address, id: &BytesN<32>, policy: &BytesN<32>) {
        self.env.mock_auths(&[MockAuth {
            address: subject,
            invoke: &MockAuthInvoke {
                contract: &self.registry,
                fn_name: "submit_attestation",
                args: (id.clone(), Symbol::new(&self.env, "gpt_4o"), policy.clone())
                    .into_val(&self.env),
                sub_invokes: &[],
            },
        }]);
    }
}

/// The contract raises errors with `panic_with_error!`, so the generated
/// client surfaces them as host errors carrying the contract error code rather
/// than as the typed enum.
fn raised(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

fn bytes(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

fn model(env: &Env) -> Symbol {
    Symbol::new(env, "gpt_4o")
}

// ---------------------------------------------------------------------------
// The subject's own authority
// ---------------------------------------------------------------------------

#[test]
fn test_subject_submitting_for_itself_succeeds() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 1);
    let policy = bytes(&f.env, 9);

    f.sign_submit(&subject, &id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &id, &model(&f.env), &policy);

    let recorded = f.client().get_attestation(&id);
    assert_eq!(recorded.subject, subject);
    assert_eq!(recorded.superseded_by, None);
    assert!(f.client().verify_attestation(&id, &subject));
}

#[test]
fn test_signature_over_different_args_is_not_reusable() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 1);

    // Signed for policy 9 — submitted with policy 10.
    f.sign_submit(&subject, &id, &bytes(&f.env, 9));
    let result = f.client().try_submit_attestation(
        &subject,
        &subject,
        &id,
        &model(&f.env),
        &bytes(&f.env, 10),
    );

    // This is the reason for require_auth_for_args over plain require_auth: a
    // signature is spendable only on the arguments it was given for.
    assert!(result.is_err());
}

#[test]
fn test_a_third_party_cannot_submit_for_a_subject_that_never_signed() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);
    let id = bytes(&f.env, 2);
    let policy = bytes(&f.env, 9);

    // The stranger signs for itself. Nobody signs for the subject.
    f.env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &f.registry,
            fn_name: "submit_attestation",
            args: (
                stranger.clone(),
                subject.clone(),
                id.clone(),
                model(&f.env),
                policy.clone(),
            )
                .into_val(&f.env),
            sub_invokes: &[],
        },
    }]);

    let result =
        f.client()
            .try_submit_attestation(&stranger, &subject, &id, &model(&f.env), &policy);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// The caller allowlist
// ---------------------------------------------------------------------------

#[test]
fn test_unallowlisted_contract_is_rejected_even_with_a_valid_subject_signature() {
    let f = setup();
    let relay = f.env.register(HonestRelay, ());
    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 3);
    let policy = bytes(&f.env, 9);

    // The subject genuinely signed, and the relay genuinely authorizes as
    // itself. The only thing missing is enrolment — and that alone must stop it.
    f.sign_submit(&subject, &id, &policy);
    let result = HonestRelayClient::new(&f.env, &relay).try_relay(
        &f.registry,
        &subject,
        &id,
        &model(&f.env),
        &policy,
    );

    assert!(result.is_err());
    assert!(!f.client().is_caller_allowed(&relay));
}

#[test]
fn test_allowlisted_contract_relaying_a_signed_submission_succeeds() {
    let f = setup();
    let relay = f.env.register(HonestRelay, ());
    f.allow(&relay);

    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 4);
    let policy = bytes(&f.env, 9);

    f.sign_submit(&subject, &id, &policy);
    HonestRelayClient::new(&f.env, &relay).relay(
        &f.registry,
        &subject,
        &id,
        &model(&f.env),
        &policy,
    );

    let recorded = f.client().get_attestation(&id);
    assert_eq!(recorded.subject, subject);
    // The submitter is recorded as the relay, and carries no authority of its own.
    assert_eq!(recorded.submitter, relay);
}

#[test]
fn test_allowlisted_contract_cannot_forge_for_a_subject_that_never_signed() {
    let f = setup();
    let relay = f.env.register(HonestRelay, ());
    f.allow(&relay);

    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 5);
    let policy = bytes(&f.env, 9);

    // Enrolment is not authority over other people. No subject signature is
    // granted here, so the relay's own standing must not substitute for one.
    f.env.mock_auths(&[]);
    let result = HonestRelayClient::new(&f.env, &relay).try_relay(
        &f.registry,
        &subject,
        &id,
        &model(&f.env),
        &policy,
    );

    assert!(result.is_err());
}

#[test]
fn test_a_contract_cannot_borrow_an_allowlisted_contract_s_standing() {
    let f = setup();
    let enrolled = f.env.register(HonestRelay, ());
    let impostor = f.env.register(HonestRelay, ());
    f.allow(&enrolled);
    assert!(!f.client().is_caller_allowed(&impostor));

    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 6);
    let policy = bytes(&f.env, 9);

    // This is the escalation the allowlist would be worthless against if
    // `caller` were taken at face value: an unenrolled contract passes the
    // enrolled contract's address and inherits its permission.
    //
    // It fails because `require_auth` on a contract address is satisfied
    // implicitly only for the *immediate invoker*. The impostor is the
    // invoker here, so authorizing for `enrolled` is not something it can do,
    // and no signature for `enrolled` exists in the transaction.
    f.sign_submit(&subject, &id, &policy);
    let result = HonestRelayClient::new(&f.env, &impostor).try_relay_claiming(
        &f.registry,
        &enrolled,
        &subject,
        &id,
        &model(&f.env),
        &policy,
    );

    assert!(result.is_err());
}

#[test]
fn test_revoking_a_caller_takes_effect_immediately() {
    let f = setup();
    let relay = f.env.register(HonestRelay, ());
    f.allow(&relay);
    assert!(f.client().is_caller_allowed(&relay));

    f.env.mock_auths(&[MockAuth {
        address: &f.admin,
        invoke: &MockAuthInvoke {
            contract: &f.registry,
            fn_name: "revoke_caller",
            args: (relay.clone(),).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.client().revoke_caller(&relay);
    assert!(!f.client().is_caller_allowed(&relay));

    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 7);
    let policy = bytes(&f.env, 9);
    f.sign_submit(&subject, &id, &policy);

    let result = HonestRelayClient::new(&f.env, &relay).try_relay(
        &f.registry,
        &subject,
        &id,
        &model(&f.env),
        &policy,
    );
    assert!(result.is_err());
}

#[test]
fn test_allowlist_administration_requires_the_admin() {
    let f = setup();
    let relay = f.env.register(HonestRelay, ());
    let impostor = Address::generate(&f.env);

    f.env.mock_auths(&[MockAuth {
        address: &impostor,
        invoke: &MockAuthInvoke {
            contract: &f.registry,
            fn_name: "allow_caller",
            args: (relay.clone(),).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);

    assert!(f.client().try_allow_caller(&relay).is_err());
    assert!(!f.client().is_caller_allowed(&relay));
}

// ---------------------------------------------------------------------------
// Attestation lifecycle
// ---------------------------------------------------------------------------

#[test]
fn test_duplicate_attestation_id_is_rejected() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let id = bytes(&f.env, 8);
    let policy = bytes(&f.env, 9);

    f.sign_submit(&subject, &id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &id, &model(&f.env), &policy);

    f.sign_submit(&subject, &id, &policy);
    let result =
        f.client()
            .try_submit_attestation(&subject, &subject, &id, &model(&f.env), &policy);

    assert_eq!(result, Err(Ok(raised(Error::AttestationExists))));
}

#[test]
fn test_verify_returns_false_for_unknown_superseded_and_mismatched_subject() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let other = Address::generate(&f.env);
    let old_id = bytes(&f.env, 20);
    let new_id = bytes(&f.env, 21);
    let policy = bytes(&f.env, 9);

    // Unknown id fails closed rather than trapping, so a caller can price
    // unattested usage instead of losing the whole transaction.
    assert!(!f.client().verify_attestation(&old_id, &subject));

    f.sign_submit(&subject, &old_id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &old_id, &model(&f.env), &policy);
    f.sign_submit(&subject, &new_id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &new_id, &model(&f.env), &policy);

    // Right id, wrong subject.
    assert!(!f.client().verify_attestation(&old_id, &other));

    f.env.mock_auths(&[MockAuth {
        address: &subject,
        invoke: &MockAuthInvoke {
            contract: &f.registry,
            fn_name: "supersede_attestation",
            args: (old_id.clone(), new_id.clone()).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.client().supersede_attestation(&subject, &old_id, &new_id);

    assert!(!f.client().verify_attestation(&old_id, &subject));
    assert!(f.client().verify_attestation(&new_id, &subject));
}

#[test]
fn test_supersede_is_authorized_by_the_subject_not_the_original_submitter() {
    let f = setup();
    let relay = f.env.register(HonestRelay, ());
    f.allow(&relay);

    let subject = Address::generate(&f.env);
    let old_id = bytes(&f.env, 30);
    let new_id = bytes(&f.env, 31);
    let policy = bytes(&f.env, 9);

    f.sign_submit(&subject, &old_id, &policy);
    HonestRelayClient::new(&f.env, &relay).relay(
        &f.registry,
        &subject,
        &old_id,
        &model(&f.env),
        &policy,
    );
    f.sign_submit(&subject, &new_id, &policy);
    HonestRelayClient::new(&f.env, &relay).relay(
        &f.registry,
        &subject,
        &new_id,
        &model(&f.env),
        &policy,
    );

    // The relay submitted both, and signs here for itself — but the record is
    // the subject's, so the submitter cannot invalidate it.
    f.env.mock_auths(&[MockAuth {
        address: &relay,
        invoke: &MockAuthInvoke {
            contract: &f.registry,
            fn_name: "supersede_attestation",
            args: (old_id.clone(), new_id.clone()).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    assert!(f
        .client()
        .try_supersede_attestation(&relay, &old_id, &new_id)
        .is_err());
    assert!(f.client().verify_attestation(&old_id, &subject));
}

#[test]
fn test_supersede_rejects_self_reference_and_double_supersede() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let old_id = bytes(&f.env, 40);
    let new_id = bytes(&f.env, 41);
    let policy = bytes(&f.env, 9);

    f.sign_submit(&subject, &old_id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &old_id, &model(&f.env), &policy);
    f.sign_submit(&subject, &new_id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &new_id, &model(&f.env), &policy);

    assert_eq!(
        f.client()
            .try_supersede_attestation(&subject, &old_id, &old_id),
        Err(Ok(raised(Error::SelfSupersede)))
    );

    let sign_supersede = |old: &BytesN<32>, new: &BytesN<32>| {
        f.env.mock_auths(&[MockAuth {
            address: &subject,
            invoke: &MockAuthInvoke {
                contract: &f.registry,
                fn_name: "supersede_attestation",
                args: (old.clone(), new.clone()).into_val(&f.env),
                sub_invokes: &[],
            },
        }]);
    };

    sign_supersede(&old_id, &new_id);
    f.client().supersede_attestation(&subject, &old_id, &new_id);

    sign_supersede(&old_id, &new_id);
    assert_eq!(
        f.client()
            .try_supersede_attestation(&subject, &old_id, &new_id),
        Err(Ok(raised(Error::AlreadySuperseded)))
    );
}

#[test]
fn test_supersede_rejects_a_replacement_for_a_different_subject() {
    let f = setup();
    let subject = Address::generate(&f.env);
    let other = Address::generate(&f.env);
    let old_id = bytes(&f.env, 50);
    let new_id = bytes(&f.env, 51);
    let policy = bytes(&f.env, 9);

    f.sign_submit(&subject, &old_id, &policy);
    f.client()
        .submit_attestation(&subject, &subject, &old_id, &model(&f.env), &policy);
    f.sign_submit(&other, &new_id, &policy);
    f.client()
        .submit_attestation(&other, &other, &new_id, &model(&f.env), &policy);

    assert_eq!(
        f.client()
            .try_supersede_attestation(&subject, &old_id, &new_id),
        Err(Ok(raised(Error::SubjectMismatch)))
    );
}

#[test]
fn test_double_initialize_is_rejected() {
    let f = setup();
    let other = Address::generate(&f.env);
    assert_eq!(
        f.client().try_initialize(&other),
        Err(Ok(raised(Error::AlreadyInitialized)))
    );
}
