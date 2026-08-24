#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Env,
};

struct Fixture {
    env: Env,
    router: Address,
    token: Address,
    payer: Address,
    payee: Address,
    arbiter: Address,
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
        arbiter,
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
}

/// Errors are raised with `panic_with_error!`, so the generated client
/// surfaces them as host errors carrying the contract error code.
fn raised(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

// ---------------------------------------------------------------------------
// Partial release against metered usage
// ---------------------------------------------------------------------------

#[test]
fn test_opening_an_escrow_moves_the_deposit_into_the_contract() {
    let f = setup(10_000);
    let id = f.open(10_000);

    assert_eq!(f.token().balance(&f.payer), 0);
    assert_eq!(f.token().balance(&f.router), 10_000);
    assert_eq!(f.client().releasable(&id), 10_000);
    assert_eq!(f.client().claimable(&id), 0);
}

#[test]
fn test_a_partial_release_settles_the_metered_portion_and_leaves_the_rest() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &2_500);

    let escrow = f.client().get_escrow(&id);
    assert_eq!(escrow.released, 2_500);
    assert_eq!(escrow.release_count, 1);
    assert_eq!(f.client().claimable(&id), 2_500);
    assert_eq!(f.client().releasable(&id), 7_500);

    // Nothing was pushed: the release only makes funds claimable, so the
    // contract still holds the whole deposit until the payee pulls.
    assert_eq!(f.token().balance(&f.payee), 0);
    assert_eq!(f.token().balance(&f.router), 10_000);
}

#[test]
fn test_the_payee_pulls_what_has_been_released() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &2_500);
    f.client().release_partial(&id, &1_500);

    let pulled = f.client().claim(&id);

    assert_eq!(pulled, 4_000);
    assert_eq!(f.token().balance(&f.payee), 4_000);
    assert_eq!(f.token().balance(&f.router), 6_000);
    assert_eq!(f.client().claimable(&id), 0);

    // A second pull with nothing newly released takes nothing.
    assert_eq!(
        f.client().try_claim(&id),
        Err(Ok(raised(Error::NothingToClaim)))
    );
}

#[test]
fn test_a_release_larger_than_the_remaining_escrow_is_rejected() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &9_000);
    assert_eq!(
        f.client().try_release_partial(&id, &1_001),
        Err(Ok(raised(Error::InsufficientEscrow)))
    );

    // Exactly the remainder is fine.
    f.client().release_partial(&id, &1_000);
    assert_eq!(f.client().releasable(&id), 0);
}

#[test]
fn test_zero_and_negative_releases_are_rejected() {
    let f = setup(10_000);
    let id = f.open(10_000);

    assert_eq!(
        f.client().try_release_partial(&id, &0),
        Err(Ok(raised(Error::InvalidAmount)))
    );
    assert_eq!(
        f.client().try_release_partial(&id, &-100),
        Err(Ok(raised(Error::InvalidAmount)))
    );
}

// ---------------------------------------------------------------------------
// Invariants
// ---------------------------------------------------------------------------

#[test]
fn test_releases_never_sum_past_the_escrowed_total() {
    let f = setup(10_000);
    let id = f.open(10_000);

    // Settle repeatedly, interleaving claims, and keep pushing past the point
    // where the deposit is exhausted. The escrow must stay inside its own
    // deposit at every step, and the contract must never pay out more than it
    // was given.
    let mut expected_released = 0i128;
    for i in 1..=MAX_RELEASES {
        let amount = 400i128;
        let result = f.client().try_release_partial(&id, &amount);

        if expected_released + amount <= 10_000 {
            assert!(result.is_ok(), "release {i} should have succeeded");
            expected_released += amount;
        } else {
            assert_eq!(result, Err(Ok(raised(Error::InsufficientEscrow))));
        }

        let escrow = f.client().get_escrow(&id);
        assert_eq!(escrow.released, expected_released);
        assert!(escrow.released <= escrow.total);
        assert!(escrow.claimed <= escrow.released);
        assert!(escrow.releasable() >= 0);

        if i % 3 == 0 && escrow.claimable() > 0 {
            f.client().claim(&id);
        }
    }

    let escrow = f.client().get_escrow(&id);
    assert!(escrow.released <= 10_000);
    if escrow.claimable() > 0 {
        f.client().claim(&id);
    }
    assert_eq!(f.token().balance(&f.payee), escrow.released);
    assert_eq!(
        f.token().balance(&f.router),
        10_000 - escrow.released,
        "the contract never pays out more than it holds"
    );
}

#[test]
fn test_the_partial_release_count_is_capped() {
    let f = setup(1_000_000);
    let id = f.open(1_000_000);

    for _ in 0..MAX_RELEASES {
        f.client().release_partial(&id, &1);
    }

    let escrow = f.client().get_escrow(&id);
    assert_eq!(escrow.release_count, MAX_RELEASES);
    // Plenty still escrowed — the cap, not the balance, is what stops this.
    assert!(escrow.releasable() > 0);
    assert_eq!(
        f.client().try_release_partial(&id, &1),
        Err(Ok(raised(Error::ReleaseCapReached)))
    );
    assert_eq!(f.client().max_releases(), MAX_RELEASES);
}

// ---------------------------------------------------------------------------
// Disputes freeze only the disputed portion
// ---------------------------------------------------------------------------

#[test]
fn test_a_dispute_does_not_touch_funds_already_released() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &3_000);
    f.client().dispute(&id, &2_000);

    let escrow = f.client().get_escrow(&id);
    assert_eq!(escrow.disputed, 2_000);
    // The earlier release was settled against usage already metered; a later
    // disagreement is not a reason to claw it back.
    assert_eq!(escrow.released, 3_000);
    assert_eq!(f.client().claimable(&id), 3_000);

    // And the payee can still take it while the dispute is open.
    assert_eq!(f.client().claim(&id), 3_000);
    assert_eq!(f.token().balance(&f.payee), 3_000);
}

#[test]
fn test_a_dispute_freezes_only_its_own_amount_leaving_the_rest_settleable() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().dispute(&id, &4_000);

    // 10_000 deposited, 4_000 frozen — the other 6_000 keeps settling normally.
    assert_eq!(f.client().releasable(&id), 6_000);
    f.client().release_partial(&id, &6_000);
    assert_eq!(f.client().claimable(&id), 6_000);

    // The frozen portion is not available to release around.
    assert_eq!(
        f.client().try_release_partial(&id, &1),
        Err(Ok(raised(Error::InsufficientEscrow)))
    );
}

#[test]
fn test_resolving_for_the_payee_makes_the_frozen_amount_claimable() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().dispute(&id, &4_000);
    f.client().resolve_dispute(&id, &true);

    let escrow = f.client().get_escrow(&id);
    assert_eq!(escrow.disputed, 0);
    assert_eq!(escrow.released, 4_000);
    // Arbitration does not consume a voluntary-settlement slot.
    assert_eq!(escrow.release_count, 0);
    assert_eq!(f.client().claim(&id), 4_000);
}

#[test]
fn test_resolving_for_the_payer_returns_the_amount_to_the_escrow() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().dispute(&id, &4_000);
    f.client().resolve_dispute(&id, &false);

    let escrow = f.client().get_escrow(&id);
    assert_eq!(escrow.disputed, 0);
    assert_eq!(escrow.released, 0);
    // Usable again rather than torn down: the relationship survives one
    // resolved disagreement.
    assert_eq!(f.client().releasable(&id), 10_000);
}

#[test]
fn test_only_the_arbiter_resolves_and_only_an_open_dispute() {
    let f = setup(10_000);
    let id = f.open(10_000);

    assert_eq!(
        f.client().try_resolve_dispute(&id, &true),
        Err(Ok(raised(Error::NoDisputeOpen)))
    );

    f.client().dispute(&id, &1_000);
    assert_eq!(
        f.client().try_dispute(&id, &1_000),
        Err(Ok(raised(Error::DisputeAlreadyOpen)))
    );

    // Auth is checked without mock_all_auths, since that would approve the
    // impostor's signature as readily as the arbiter's.
    let impostor = Address::generate(&f.env);
    f.env.mock_auths(&[MockAuth {
        address: &impostor,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "resolve_dispute",
            args: (id, true).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    assert!(f.client().try_resolve_dispute(&id, &true).is_err());
    assert_eq!(f.client().get_escrow(&id).disputed, 1_000);

    f.env.mock_auths(&[MockAuth {
        address: &f.arbiter,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "resolve_dispute",
            args: (id, true).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.client().resolve_dispute(&id, &true);
    assert_eq!(f.client().get_escrow(&id).disputed, 0);
}

// ---------------------------------------------------------------------------
// Closing
// ---------------------------------------------------------------------------

#[test]
fn test_closing_refunds_the_remainder_and_leaves_released_funds_claimable() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().release_partial(&id, &2_500);
    f.client().close_escrow(&id);

    // Unreleased remainder goes back to the payer...
    assert_eq!(f.token().balance(&f.payer), 7_500);
    // ...and the payee is not stranded by the close.
    assert_eq!(f.client().claim(&id), 2_500);
    assert_eq!(f.token().balance(&f.payee), 2_500);
    assert_eq!(f.token().balance(&f.router), 0);

    assert_eq!(
        f.client().try_release_partial(&id, &1),
        Err(Ok(raised(Error::EscrowClosed)))
    );
}

#[test]
fn test_closing_is_refused_while_a_dispute_is_open() {
    let f = setup(10_000);
    let id = f.open(10_000);

    f.client().dispute(&id, &4_000);
    // Closing here would refund the frozen amount out from under arbitration.
    assert_eq!(
        f.client().try_close_escrow(&id),
        Err(Ok(raised(Error::DisputeAlreadyOpen)))
    );
}

#[test]
fn test_an_escrow_needs_two_distinct_parties_and_a_positive_amount() {
    let f = setup(10_000);

    assert_eq!(
        f.client()
            .try_open_escrow(&f.payer, &f.payer, &f.token, &1_000),
        Err(Ok(raised(Error::SamePayerAndPayee)))
    );
    assert_eq!(
        f.client().try_open_escrow(&f.payer, &f.payee, &f.token, &0),
        Err(Ok(raised(Error::InvalidAmount)))
    );
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[test]
fn test_the_payer_signature_is_bound_to_the_release_amount() {
    let f = setup(10_000);
    let id = f.open(10_000);

    // Signed to settle 1_000.
    f.env.mock_auths(&[MockAuth {
        address: &f.payer,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "release_partial",
            args: (id, 1_000i128).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);

    // Submitted to settle 9_000. Binding the signature to the amount is what
    // stops a release the payer agreed to from being enlarged in transit.
    assert!(f.client().try_release_partial(&id, &9_000).is_err());
    assert_eq!(f.client().get_escrow(&id).released, 0);

    f.env.mock_auths(&[MockAuth {
        address: &f.payer,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "release_partial",
            args: (id, 1_000i128).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.client().release_partial(&id, &1_000);
    assert_eq!(f.client().get_escrow(&id).released, 1_000);
}

#[test]
fn test_only_the_payee_can_pull() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &1_000);

    let stranger = Address::generate(&f.env);
    f.env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "claim",
            args: (id,).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);

    assert!(f.client().try_claim(&id).is_err());
    assert_eq!(f.token().balance(&stranger), 0);
    assert_eq!(f.client().claimable(&id), 1_000);
}
