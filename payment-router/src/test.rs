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

// ---------------------------------------------------------------------------
// Failed settlement, recovery, and the dead-letter path
// ---------------------------------------------------------------------------

/// Advance the ledger far enough for the dead-letter window to open.
fn advance(f: &Fixture, ledgers: u32) {
    use soroban_sdk::testutils::Ledger as _;
    let at = f.env.ledger().sequence();
    f.env.ledger().set_sequence_number(at + ledgers);
}

#[test]
fn test_a_healthy_escrow_reports_zeroed_recovery_state() {
    let f = setup(10_000);
    let id = f.open(10_000);

    let state = f.client().settlement_status(&id);
    assert_eq!(state.failures, 0);
    assert!(!state.failed);
    assert!(!state.dead_lettered);
    // Nothing to recover from, so no dead-letter deadline exists yet.
    assert_eq!(f.client().dead_letter_at(&id), None);
}

#[test]
fn test_a_reported_failure_is_classified_and_preserves_the_claim() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);

    assert_eq!(
        f.client().report_failure(&id, &FailureReason::NoTrustline),
        1
    );

    let state = f.client().settlement_status(&id);
    assert_eq!(state.failures, 1);
    assert_eq!(state.last_reason, FailureReason::NoTrustline);
    // Below the threshold, so still healthy - and the claim is untouched.
    assert!(!state.failed);
    assert_eq!(f.client().claimable(&id), 3_000);
    assert_eq!(f.token().balance(&f.router), 10_000);
}

#[test]
fn test_the_escrow_enters_the_failed_state_after_the_third_attempt() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);

    for expected in 1..=f.client().max_settlement_failures() {
        assert_eq!(
            f.client()
                .report_failure(&id, &FailureReason::NoDestination),
            expected
        );
    }

    let state = f.client().settlement_status(&id);
    assert!(state.failed);
    assert_eq!(state.failed_at, f.env.ledger().sequence());
    // The value is still the payee's - Failed preserves the claim.
    assert_eq!(f.client().claimable(&id), 3_000);
    assert_eq!(
        f.client().dead_letter_at(&id),
        Some(state.failed_at + 120_960)
    );
}

#[test]
fn test_a_payee_who_fixes_their_account_pulls_normally_and_clears_the_record() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);

    for _ in 0..3 {
        f.client().report_failure(&id, &FailureReason::NoTrustline);
    }
    assert!(f.client().settlement_status(&id).failed);

    // Recovery is pull-based: the ordinary claim, once the account works.
    f.client().clear_failure(&id);
    assert_eq!(f.client().claim(&id), 3_000);
    assert_eq!(f.token().balance(&f.payee), 3_000);

    let state = f.client().settlement_status(&id);
    assert!(!state.failed);
    assert_eq!(state.failures, 0);
    assert_eq!(f.client().dead_letter_at(&id), None);
}

#[test]
fn test_reporting_a_failure_needs_something_actually_owed() {
    let f = setup(10_000);
    let id = f.open(10_000);

    // Nothing released, so no settlement could have been attempted.
    assert_eq!(
        f.client().try_report_failure(&id, &FailureReason::Other),
        Err(Ok(raised(Error::NothingToClaim)))
    );
}

#[test]
fn test_only_the_payee_can_report_a_failure_on_their_own_escrow() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);

    let stranger = Address::generate(&f.env);
    f.env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "report_failure",
            args: (id,).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);

    assert!(f
        .client()
        .try_report_failure(&id, &FailureReason::Other)
        .is_err());
    assert_eq!(f.client().settlement_status(&id).failures, 0);
}

#[test]
fn test_the_dead_letter_path_is_shut_until_the_delay_elapses() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);
    for _ in 0..3 {
        f.client()
            .report_failure(&id, &FailureReason::AssetRestricted);
    }

    // The window exists for the payee's benefit: a frozen asset is fixable.
    assert_eq!(
        f.client().try_dead_letter(&id),
        Err(Ok(raised(Error::DeadLetterNotReady)))
    );
    assert_eq!(f.client().claimable(&id), 3_000);

    advance(&f, 120_959);
    assert_eq!(
        f.client().try_dead_letter(&id),
        Err(Ok(raised(Error::DeadLetterNotReady)))
    );
}

#[test]
fn test_dead_lettering_returns_the_value_to_the_payer_and_ends_the_claim() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);
    for _ in 0..3 {
        f.client()
            .report_failure(&id, &FailureReason::NoDestination);
    }
    advance(&f, 120_960);

    assert_eq!(f.client().dead_letter(&id), 3_000);

    // Policy: back to the party that put it in - the only destination that
    // needs no new trust assumption. Not burned, not kept by the contract.
    assert_eq!(f.token().balance(&f.payer), 3_000);
    assert_eq!(f.token().balance(&f.payee), 0);
    assert_eq!(f.token().balance(&f.router), 7_000);

    // And the swept value is no longer claimable, so it cannot be paid twice.
    assert!(f.client().settlement_status(&id).dead_lettered);
    assert_eq!(f.client().claimable(&id), 0);
    assert_eq!(
        f.client().try_claim(&id),
        Err(Ok(raised(Error::NothingToClaim)))
    );
}

#[test]
fn test_a_dead_lettered_escrow_cannot_be_swept_or_reported_again() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);
    for _ in 0..3 {
        f.client().report_failure(&id, &FailureReason::Other);
    }
    advance(&f, 120_960);
    f.client().dead_letter(&id);

    let payer_after = f.token().balance(&f.payer);
    assert_eq!(
        f.client().try_dead_letter(&id),
        Err(Ok(raised(Error::AlreadyDeadLettered)))
    );
    assert_eq!(
        f.client().try_report_failure(&id, &FailureReason::Other),
        Err(Ok(raised(Error::AlreadyDeadLettered)))
    );
    assert_eq!(f.token().balance(&f.payer), payer_after);
}

#[test]
fn test_dead_lettering_a_healthy_escrow_is_refused() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);

    assert_eq!(
        f.client().try_dead_letter(&id),
        Err(Ok(raised(Error::SettlementNotFailed)))
    );
    assert_eq!(
        f.client().try_clear_failure(&id),
        Err(Ok(raised(Error::SettlementNotFailed)))
    );
}

#[test]
fn test_the_dead_letter_sweep_is_the_arbiters_call_not_the_payers() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &3_000);
    for _ in 0..3 {
        f.client().report_failure(&id, &FailureReason::NoTrustline);
    }
    advance(&f, 120_960);

    // The payer is the beneficiary of the sweep, so letting them trigger it
    // would reward griefing a payee into the failed state.
    f.env.mock_auths(&[MockAuth {
        address: &f.payer,
        invoke: &MockAuthInvoke {
            contract: &f.router,
            fn_name: "dead_letter",
            args: (id,).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    assert!(f.client().try_dead_letter(&id).is_err());
    assert_eq!(f.client().claimable(&id), 3_000);

    f.env.mock_all_auths();
    assert_eq!(f.client().dead_letter(&id), 3_000);
}

#[test]
fn test_failure_handling_does_not_disturb_the_arbiters_dispute_path() {
    let f = setup(10_000);
    let id = f.open(10_000);
    f.client().release_partial(&id, &2_000);
    f.client().report_failure(&id, &FailureReason::NoTrustline);

    // A failing payout on the released portion is unrelated to a dispute over
    // the remainder; both must still work.
    f.client().dispute(&id, &5_000);
    f.client().resolve_dispute(&id, &false);
    assert_eq!(f.client().releasable(&id), 8_000);
    assert_eq!(f.client().settlement_status(&id).failures, 1);
}
