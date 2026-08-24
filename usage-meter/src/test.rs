#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Env,
};

/// Stands in for `audit-registry`, implementing only the one method this
/// contract calls. Using a stub rather than depending on the real crate keeps
/// this test suite about the *link* — what happens when verification passes,
/// fails, or is replayed — rather than about attestation semantics that belong
/// to the registry's own tests.
#[contract]
pub struct StubRegistry;

#[contractimpl]
impl StubRegistry {
    pub fn attest(env: Env, id: BytesN<32>, subject: Address) {
        env.storage().persistent().set(&id, &subject);
    }

    pub fn verify_attestation(env: Env, id: BytesN<32>, subject: Address) -> bool {
        match env.storage().persistent().get::<_, Address>(&id) {
            Some(s) => s == subject,
            None => false,
        }
    }
}

struct Fixture {
    env: Env,
    meter: Address,
    registry: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let meter = env.register(UsageMeter, ());
    let registry = env.register(StubRegistry, ());

    let client = UsageMeterClient::new(&env, &meter);
    client.initialize(&admin);
    client.set_registry(&registry);

    Fixture {
        env,
        meter,
        registry,
    }
}

impl Fixture {
    fn client(&self) -> UsageMeterClient<'_> {
        UsageMeterClient::new(&self.env, &self.meter)
    }

    fn stub(&self) -> StubRegistryClient<'_> {
        StubRegistryClient::new(&self.env, &self.registry)
    }

    /// Register a valid attestation for `payer` in the stub registry.
    fn attest(&self, id: &BytesN<32>, payer: &Address) {
        self.stub().attest(id, payer);
    }
}

/// Errors are raised with `panic_with_error!`, so the generated client
/// surfaces them as host errors carrying the contract error code.
fn raised(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

fn bytes(env: &Env, b: u8) -> BytesN<32> {
    BytesN::from_array(env, &[b; 32])
}

// ---------------------------------------------------------------------------
// The link itself
// ---------------------------------------------------------------------------

#[test]
fn test_usage_backed_by_a_verified_attestation_is_recorded_as_attested() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 1);
    f.attest(&id, &payer);

    let record_id = f.client().record_usage(&payer, &Some(id.clone()), &1_000);
    let record = f.client().get_usage(&record_id);

    assert!(record.attested);
    assert_eq!(record.attestation_id, Some(id.clone()));
    assert_eq!(record.units, 1_000);
    assert_eq!(f.client().attested_units(&payer), 1_000);
    assert_eq!(f.client().unattested_units(&payer), 0);
    assert!(f.client().is_metered(&id));
}

#[test]
fn test_the_same_attestation_cannot_be_metered_twice() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 2);
    f.attest(&id, &payer);

    f.client().record_usage(&payer, &Some(id.clone()), &500);
    let result = f.client().try_record_usage(&payer, &Some(id.clone()), &500);

    assert_eq!(result, Err(Ok(raised(Error::AlreadyMetered))));
    // The first record stands; the replay added nothing.
    assert_eq!(f.client().attested_units(&payer), 500);
}

#[test]
fn test_an_attestation_belonging_to_someone_else_does_not_verify() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let other = Address::generate(&f.env);
    let id = bytes(&f.env, 3);
    f.attest(&id, &other);

    // Verification is against the claimed payer, so an attestation that exists
    // but belongs elsewhere is no better than none — under the strict default
    // that means rejected, not silently billed.
    let result = f.client().try_record_usage(&payer, &Some(id.clone()), &10);

    assert_eq!(result, Err(Ok(raised(Error::AttestationRequired))));
    assert!(!f.client().is_metered(&id));
}

#[test]
fn test_an_unknown_attestation_id_is_not_burned_by_the_failed_attempt() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 4);

    // Metering an id that does not verify must not mark it used — otherwise a
    // typo, or a race against the registry write, would permanently block the
    // real attestation that later occupies that id.
    assert!(f
        .client()
        .try_record_usage(&payer, &Some(id.clone()), &10)
        .is_err());
    assert!(!f.client().is_metered(&id));

    f.attest(&id, &payer);
    let record_id = f.client().record_usage(&payer, &Some(id.clone()), &10);
    assert!(f.client().get_usage(&record_id).attested);
}

// ---------------------------------------------------------------------------
// Unattested policy
// ---------------------------------------------------------------------------

#[test]
fn test_unattested_usage_is_rejected_by_default() {
    let f = setup();
    let payer = Address::generate(&f.env);

    // Nothing was configured for this payer. The default has to be the strict
    // one — a payer must opt in to being billed for usage nothing vouches for.
    assert_eq!(
        f.client().unattested_policy(&payer),
        UnattestedPolicy::Reject
    );
    assert_eq!(
        f.client().try_record_usage(&payer, &None, &10),
        Err(Ok(raised(Error::AttestationRequired)))
    );
}

#[test]
fn test_opting_in_records_unattested_usage_against_a_separate_total() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 5);
    f.attest(&id, &payer);

    f.client()
        .set_unattested_policy(&payer, &UnattestedPolicy::RecordUnattested);

    f.client().record_usage(&payer, &Some(id), &700);
    let unattested_id = f.client().record_usage(&payer, &None, &300);
    let record = f.client().get_usage(&unattested_id);

    assert!(!record.attested);
    assert_eq!(record.attestation_id, None);
    // The two totals stay apart so downstream pricing can treat them
    // differently rather than having to re-derive which was which.
    assert_eq!(f.client().attested_units(&payer), 700);
    assert_eq!(f.client().unattested_units(&payer), 300);
}

#[test]
fn test_a_failed_verification_under_the_lenient_policy_drops_the_reference() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 6);

    f.client()
        .set_unattested_policy(&payer, &UnattestedPolicy::RecordUnattested);
    let record_id = f.client().record_usage(&payer, &Some(id.clone()), &42);
    let record = f.client().get_usage(&record_id);

    // Recorded, but not as attested, and without keeping the id it failed to
    // verify — a record that carried the reference would read as sourced.
    assert!(!record.attested);
    assert_eq!(record.attestation_id, None);
    assert_eq!(f.client().unattested_units(&payer), 42);
    assert!(!f.client().is_metered(&id));
}

#[test]
fn test_policy_is_per_payer_and_does_not_leak_across_payers() {
    let f = setup();
    let lenient = Address::generate(&f.env);
    let strict = Address::generate(&f.env);

    f.client()
        .set_unattested_policy(&lenient, &UnattestedPolicy::RecordUnattested);

    assert_eq!(
        f.client().unattested_policy(&strict),
        UnattestedPolicy::Reject
    );
    f.client().record_usage(&lenient, &None, &5);
    assert_eq!(
        f.client().try_record_usage(&strict, &None, &5),
        Err(Ok(raised(Error::AttestationRequired)))
    );
}

// ---------------------------------------------------------------------------
// Authorization
//
// These two do not use mock_all_auths: they are about who may authorize what,
// and mock_all_auths approves every require_auth in the transaction, which
// would make them pass no matter what the contract checked.
// ---------------------------------------------------------------------------

#[test]
fn test_the_payer_signature_is_bound_to_the_unit_count() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let meter = env.register(UsageMeter, ());
    let registry = env.register(StubRegistry, ());
    let payer = Address::generate(&env);
    let id = bytes(&env, 7);

    env.mock_all_auths();
    let client = UsageMeterClient::new(&env, &meter);
    client.initialize(&admin);
    client.set_registry(&registry);
    StubRegistryClient::new(&env, &registry).attest(&id, &payer);

    // Signed for 100 units.
    env.mock_auths(&[MockAuth {
        address: &payer,
        invoke: &MockAuthInvoke {
            contract: &meter,
            fn_name: "record_usage",
            args: (Some(id.clone()), 100u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    // Submitted for 10_000. An intermediate that inflates the meter reading
    // after the payer signed is exactly what require_auth_for_args stops.
    assert!(client
        .try_record_usage(&payer, &Some(id.clone()), &10_000)
        .is_err());

    env.mock_auths(&[MockAuth {
        address: &payer,
        invoke: &MockAuthInvoke {
            contract: &meter,
            fn_name: "record_usage",
            args: (Some(id.clone()), 100u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.record_usage(&payer, &Some(id), &100);
    assert_eq!(client.attested_units(&payer), 100);
}

#[test]
fn test_only_the_payer_can_relax_their_own_policy() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let meter = env.register(UsageMeter, ());
    let payer = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &meter,
            fn_name: "initialize",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let client = UsageMeterClient::new(&env, &meter);
    client.initialize(&admin);

    // The admin signs. The policy decides whether this payer can be billed for
    // unvouched usage, so the admin's signature must not be enough.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &meter,
            fn_name: "set_unattested_policy",
            args: (UnattestedPolicy::RecordUnattested,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client
        .try_set_unattested_policy(&payer, &UnattestedPolicy::RecordUnattested)
        .is_err());
    assert_eq!(client.unattested_policy(&payer), UnattestedPolicy::Reject);
}

// ---------------------------------------------------------------------------
// Guards and cost
// ---------------------------------------------------------------------------

#[test]
fn test_zero_units_is_rejected() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 8);
    f.attest(&id, &payer);

    assert_eq!(
        f.client().try_record_usage(&payer, &Some(id), &0),
        Err(Ok(raised(Error::ZeroUnits)))
    );
}

#[test]
fn test_recording_without_a_registry_configured_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let meter = env.register(UsageMeter, ());
    let client = UsageMeterClient::new(&env, &meter);
    client.initialize(&admin);

    let payer = Address::generate(&env);
    assert_eq!(
        client.try_record_usage(&payer, &Some(bytes(&env, 9)), &10),
        Err(Ok(raised(Error::RegistryNotSet)))
    );
}

/// Measures what the cross-contract verification actually costs, which the
/// issue asks to be quantified before this link is relied on.
///
/// Measured on soroban-sdk 27.0.6, comparing an attested write (one
/// cross-contract call into the registry) against an unattested one (no call),
/// same contract, same storage writes otherwise:
///
/// | write | CPU instructions | memory bytes |
/// |---|---|---|
/// | unattested (no call) | 186,318 | 77,223 |
/// | attested (one call) | 256,825 | 97,795 |
/// | **verification** | **70,507** | **20,572** |
///
/// About 0.07% of the 100,000,000-instruction transaction budget, so the
/// per-record call is affordable and the batch-root alternative the issue
/// raises is not needed at this cost. The assertion below is a
/// direction-and-magnitude check rather than an exact figure, so an SDK upgrade
/// does not fail CI on a number — run with `--nocapture` for current values.
#[test]
fn test_cross_contract_verification_cost_is_measured() {
    let f = setup();
    let payer = Address::generate(&f.env);
    let id = bytes(&f.env, 10);
    f.attest(&id, &payer);
    f.client()
        .set_unattested_policy(&payer, &UnattestedPolicy::RecordUnattested);

    let mut budget = f.env.cost_estimate().budget();

    budget.reset_default();
    f.client().record_usage(&payer, &None, &1);
    let without_cpu = budget.cpu_instruction_cost();
    let without_mem = budget.memory_bytes_cost();

    budget.reset_default();
    f.client().record_usage(&payer, &Some(id), &1);
    let with_cpu = budget.cpu_instruction_cost();
    let with_mem = budget.memory_bytes_cost();

    std::println!("unattested write:  cpu={without_cpu} mem={without_mem}");
    std::println!("attested write:    cpu={with_cpu} mem={with_mem}");
    std::println!(
        "verification cost: cpu={} mem={}",
        with_cpu - without_cpu,
        with_mem - without_mem
    );

    // The call is not free, and it is not so expensive that a single
    // verification would crowd out the rest of an invocation's budget. If this
    // ever fails, the batch-root alternative in the issue is worth revisiting.
    assert!(with_cpu > without_cpu);
    assert!(with_cpu - without_cpu < 10_000_000);
}
