#![cfg(test)]
//! Value-safety invariants for the settlement paths.
//!
//! These are the properties an auditor checks first, so they are asserted
//! directly rather than being implied by the behavioural tests in `test`:
//!
//! 1. **Conservation.** The router's token balance always equals the sum of
//!    what every escrow still owes — nothing is created, nothing evaporates.
//! 2. **No escrow pays out more than it took in.** `claimed <= released <=
//!    total` on every path, including after a dispute and after a close.
//! 3. **Settled paths are idempotent.** A second `close_escrow` or a second
//!    `claim` on a settled escrow returns an error and moves no funds.
//! 4. **A hostile token cannot re-enter a value path.** The token contract is
//!    supplied by the payer and is therefore untrusted code on the callback.

use super::*;
use soroban_sdk::{
    contract as test_contract, contractimpl as test_contractimpl,
    testutils::Address as _,
    token::{Client as TokenClient, StellarAssetClient},
    Env,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Fixture {
    env: Env,
    router: Address,
    token: Address,
    payer: Address,
    payee: Address,
}

fn setup(funding: i128) -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let arbiter = Address::generate(&env);
    let router = env.register(PaymentRouter, ());
    PaymentRouterClient::new(&env, &router).initialize(&arbiter);

    let issuer = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(issuer).address();

    let payer = Address::generate(&env);
    let payee = Address::generate(&env);
    StellarAssetClient::new(&env, &token).mint(&payer, &funding);

    Fixture {
        env,
        router,
        token,
        payer,
        payee,
    }
}

impl Fixture {
    fn client(&self) -> PaymentRouterClient<'_> {
        PaymentRouterClient::new(&self.env, &self.router)
    }

    fn token(&self) -> TokenClient<'_> {
        TokenClient::new(&self.env, &self.token)
    }

    fn open(&self, total: i128) -> u64 {
        self.client()
            .open_escrow(&self.payer, &self.payee, &self.token, &total)
    }

    /// What the router still owes on one escrow: the released-but-unpulled
    /// portion, plus whatever remains escrowed or frozen against it.
    fn outstanding(&self, id: u64) -> i128 {
        let e = self.client().get_escrow(&id);
        let unreleased = if e.closed {
            0
        } else {
            e.total - e.released - e.disputed
        };
        (e.released - e.claimed) + unreleased + if e.closed { 0 } else { e.disputed }
    }

    /// Conservation: the contract holds exactly what it still owes, and no
    /// escrow has paid out more than was ever deposited into it.
    fn assert_conserved(&self, ids: &[u64]) {
        let owed: i128 = ids.iter().map(|id| self.outstanding(*id)).sum();
        assert_eq!(
            self.token().balance(&self.router),
            owed,
            "router balance must equal the sum of outstanding escrow claims"
        );

        for id in ids {
            let e = self.client().get_escrow(id);
            assert!(
                e.claimed <= e.released,
                "escrow {id} paid out unreleased funds"
            );
            assert!(
                e.released + e.disputed <= e.total,
                "escrow {id} released more than it took in"
            );
            assert!(e.claimed >= 0 && e.released >= 0 && e.disputed >= 0);
        }
    }
}

fn raised(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

// ---------------------------------------------------------------------------
// Conservation
// ---------------------------------------------------------------------------

#[test]
fn test_balances_are_conserved_across_a_full_escrow_lifecycle() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.assert_conserved(&[id]);

    f.client().release_partial(&id, &2_500);
    f.assert_conserved(&[id]);

    f.client().claim(&id);
    f.assert_conserved(&[id]);

    f.client().release_partial(&id, &1_500);
    f.assert_conserved(&[id]);

    f.client().close_escrow(&id);
    f.assert_conserved(&[id]);

    f.client().claim(&id);
    f.assert_conserved(&[id]);

    // Every unit deposited ends up with exactly one party.
    assert_eq!(f.token().balance(&f.payee), 4_000);
    assert_eq!(f.token().balance(&f.payer), 6_000);
    assert_eq!(f.token().balance(&f.router), 0);
}

#[test]
fn test_balances_are_conserved_across_a_dispute() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &1_000);
    f.client().dispute(&id, &4_000);
    f.assert_conserved(&[id]);

    f.client().resolve_dispute(&id, &true);
    f.assert_conserved(&[id]);

    f.client().claim(&id);
    f.assert_conserved(&[id]);
    assert_eq!(f.token().balance(&f.payee), 5_000);
}

#[test]
fn test_conservation_holds_with_several_escrows_sharing_the_contract_balance() {
    let f = setup(30_000);
    let a = f.open(10_000);
    let b = f.open(12_000);
    let c = f.open(8_000);

    f.client().release_partial(&a, &4_000);
    f.client().release_partial(&b, &12_000);
    f.client().claim(&b);
    f.client().close_escrow(&c);

    f.assert_conserved(&[a, b, c]);
    assert_eq!(f.token().balance(&f.router), 10_000);
}

// ---------------------------------------------------------------------------
// Idempotence of the settled paths
// ---------------------------------------------------------------------------

#[test]
fn test_a_second_close_returns_already_settled_and_refunds_nothing() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &2_000);
    f.client().close_escrow(&id);

    let payer_after_first = f.token().balance(&f.payer);
    assert_eq!(payer_after_first, 8_000);

    assert_eq!(
        f.client().try_close_escrow(&id),
        Err(Ok(raised(Error::AlreadySettled)))
    );
    // The decisive assertion: the error is not merely returned, no value moved.
    assert_eq!(f.token().balance(&f.payer), payer_after_first);
    assert_eq!(f.token().balance(&f.router), 2_000);
    f.assert_conserved(&[id]);
}

#[test]
fn test_a_second_claim_on_a_settled_escrow_returns_already_settled() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &3_000);
    f.client().close_escrow(&id);
    assert_eq!(f.client().claim(&id), 3_000);

    assert_eq!(
        f.client().try_claim(&id),
        Err(Ok(raised(Error::AlreadySettled)))
    );
    assert_eq!(f.token().balance(&f.payee), 3_000);
    assert_eq!(f.token().balance(&f.router), 0);
}

#[test]
fn test_an_open_escrow_with_nothing_released_still_reports_nothing_to_claim() {
    let f = setup(10_000);
    let id = f.open(10_000);

    // Distinct from AlreadySettled: this one is worth retrying later.
    assert_eq!(
        f.client().try_claim(&id),
        Err(Ok(raised(Error::NothingToClaim)))
    );
}

#[test]
fn test_releases_can_never_sum_past_the_deposit() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &6_000);
    assert_eq!(
        f.client().try_release_partial(&id, &4_001),
        Err(Ok(raised(Error::InsufficientEscrow)))
    );
    f.client().release_partial(&id, &4_000);
    assert_eq!(
        f.client().try_release_partial(&id, &1),
        Err(Ok(raised(Error::InsufficientEscrow)))
    );
    f.assert_conserved(&[id]);
}

// ---------------------------------------------------------------------------
// Adversarial token
// ---------------------------------------------------------------------------

/// A token whose `transfer` calls back into the router.
///
/// The payer chooses the token address when opening an escrow, so this is code
/// the router must assume is hostile. Only `transfer` is implemented because
/// that is the whole of the router's dependency on the token interface.
#[test_contract]
pub struct HostileToken;

#[contracttype]
#[derive(Clone)]
pub enum Hostile {
    Target,
    Escrow,
    Armed,
}

#[test_contractimpl]
impl HostileToken {
    /// Point the callback at a router and escrow. Disarmed until `arm` is
    /// called, so the deposit that funds the escrow can complete normally.
    pub fn arm(env: Env, router: Address, id: u64) {
        env.storage().instance().set(&Hostile::Target, &router);
        env.storage().instance().set(&Hostile::Escrow, &id);
        env.storage().instance().set(&Hostile::Armed, &true);
    }

    pub fn transfer(env: Env, _from: Address, _to: Address, _amount: i128) {
        let armed: bool = env
            .storage()
            .instance()
            .get(&Hostile::Armed)
            .unwrap_or(false);
        if !armed {
            return;
        }
        // Fire once, so the callback cannot recurse forever on its own.
        env.storage().instance().set(&Hostile::Armed, &false);

        let router: Address = env.storage().instance().get(&Hostile::Target).unwrap();
        let id: u64 = env.storage().instance().get(&Hostile::Escrow).unwrap();
        // The double-release attempt: pull again from inside the first pull.
        PaymentRouterClient::new(&env, &router).claim(&id);
    }
}

#[test]
fn test_a_token_that_calls_back_during_a_claim_cannot_pull_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let arbiter = Address::generate(&env);
    let router = env.register(PaymentRouter, ());
    let client = PaymentRouterClient::new(&env, &router);
    client.initialize(&arbiter);

    let token = env.register(HostileToken, ());
    let payer = Address::generate(&env);
    let payee = Address::generate(&env);

    // The deposit runs while the token is disarmed.
    let id = client.open_escrow(&payer, &payee, &token, &10_000);
    client.release_partial(&id, &4_000);

    HostileTokenClient::new(&env, &token).arm(&router, &id);

    // The payout re-enters. It is refused, which fails the outer call too —
    // a hostile token can break its own settlement, but it cannot double-pull.
    assert!(client.try_claim(&id).is_err());

    // Nothing was recorded: the reverted transaction took the write with it.
    let escrow = client.get_escrow(&id);
    assert_eq!(escrow.claimed, 0);
    assert_eq!(escrow.released, 4_000);
    assert_eq!(client.claimable(&id), 4_000);
}

#[test]
fn test_a_token_that_calls_back_during_a_deposit_cannot_drain_another_escrow() {
    let env = Env::default();
    env.mock_all_auths();

    let arbiter = Address::generate(&env);
    let router = env.register(PaymentRouter, ());
    let client = PaymentRouterClient::new(&env, &router);
    client.initialize(&arbiter);

    let token = env.register(HostileToken, ());
    let payer = Address::generate(&env);
    let payee = Address::generate(&env);

    let victim = client.open_escrow(&payer, &payee, &token, &5_000);
    client.release_partial(&victim, &5_000);

    // Arm before opening the second escrow: the callback now fires from inside
    // the deposit, at the moment the new escrow row exists but is unfunded.
    HostileTokenClient::new(&env, &token).arm(&router, &victim);

    assert!(client
        .try_open_escrow(&payer, &payee, &token, &1_000)
        .is_err());

    // The victim escrow is untouched.
    assert_eq!(client.get_escrow(&victim).claimed, 0);
    assert_eq!(client.claimable(&victim), 5_000);
}
