#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::{StellarAssetClient, TokenClient as TestTokenClient},
    Address, Env, String,
};

const HOUR: u64 = 3_600;
#[allow(dead_code)]
const DAY: u64 = 86_400;
const FEE: i128 = 1_000;
const BOND: i128 = 500;
const PLATFORM_BPS: u32 = 500; // 5%
const INITIAL_BALANCE: i128 = 100_000;

#[allow(dead_code)]
struct Setup<'a> {
    env: Env,
    client: DeliveryEscrowContractClient<'a>,
    contract_addr: Address,
    admin: Address,
    customer: Address,
    rider: Address,
    platform_wallet: Address,
    token: TestTokenClient<'a>,
    token_admin: StellarAssetClient<'a>,
    token_addr: Address,
}

fn setup() -> Setup<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let platform_wallet = Address::generate(&env);

    let token_admin_addr = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin_addr.clone());
    let token_addr = sac.address();
    let token = TestTokenClient::new(&env, &token_addr);
    let token_admin = StellarAssetClient::new(&env, &token_addr);

    let contract_id = env.register(
        DeliveryEscrowContract,
        (&admin, &token_addr, &platform_wallet),
    );
    let client = DeliveryEscrowContractClient::new(&env, &contract_id);

    let customer = Address::generate(&env);
    let rider = Address::generate(&env);

    token_admin.mint(&customer, &INITIAL_BALANCE);
    token_admin.mint(&rider, &INITIAL_BALANCE);

    Setup {
        env,
        client,
        contract_addr: contract_id,
        admin,
        customer,
        rider,
        platform_wallet,
        token,
        token_admin,
        token_addr,
    }
}

fn advance(env: &Env, by_secs: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp = li.timestamp.saturating_add(by_secs);
    });
}

fn create_default(s: &Setup) -> Order {
    s.client.create_order(
        &s.customer,
        &s.rider,
        &FEE,
        &BOND,
        &PLATFORM_BPS,
        &(24 * HOUR),
        &(48 * HOUR),
        &String::from_str(&s.env, "ipfs://order-meta"),
    )
}

fn approve_both_parties(s: &Setup) {
    // Customer pre-approves the contract for the delivery fee.
    s.token.approve(
        &s.customer,
        &s.contract_addr,
        &FEE,
        &(s.env.ledger().sequence() + 1_000_000),
    );
    // Rider pre-approves the contract for the bond.
    s.token.approve(
        &s.rider,
        &s.contract_addr,
        &BOND,
        &(s.env.ledger().sequence() + 1_000_000),
    );
}

// ---------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------

#[test]
fn test_create_order_emits_open_state() {
    let s = setup();
    let order = create_default(&s);
    assert_eq!(order.status, OrderStatus::Open);
    assert_eq!(order.delivery_fee, FEE);
    assert_eq!(order.rider_bond, BOND);
}

#[test]
fn test_fund_moves_to_funded_state() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);

    let funded = s.client.fund(&order.order_id);
    assert_eq!(funded.status, OrderStatus::Funded);
    // Contract holds the pot.
    assert_eq!(s.token.balance(&s.contract_addr), FEE + BOND);
    // Customer down by FEE, rider down by BOND.
    assert_eq!(s.token.balance(&s.customer), INITIAL_BALANCE - FEE);
    assert_eq!(s.token.balance(&s.rider), INITIAL_BALANCE - BOND);
}

#[test]
fn test_full_happy_path_customer_confirms() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);

    let alloc = s.client.confirm_delivery(&s.customer, &order.order_id);
    let platform_cut = FEE * PLATFORM_BPS as i128 / BPS_DENOMINATOR;
    let rider_take = FEE - platform_cut + BOND;
    assert_eq!(alloc.to_rider, rider_take);
    assert_eq!(alloc.to_platform, platform_cut);

    // Final balances:
    assert_eq!(
        s.token.balance(&s.rider),
        INITIAL_BALANCE - BOND + rider_take
    );
    assert_eq!(s.token.balance(&s.platform_wallet), platform_cut);
    assert_eq!(s.token.balance(&s.customer), INITIAL_BALANCE - FEE);
    assert_eq!(s.token.balance(&s.contract_addr), 0);

    let post = s.client.get_order(&order.order_id);
    assert_eq!(post.status, OrderStatus::Released);
}

#[test]
fn test_claim_after_timeout() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);

    advance(&s.env, 24 * HOUR + 1);
    assert!(s.client.is_timeout_claimable(&order.order_id));

    let alloc = s.client.claim_after_timeout(&s.rider, &order.order_id);
    let platform_cut = FEE * PLATFORM_BPS as i128 / BPS_DENOMINATOR;
    assert_eq!(alloc.to_rider, FEE - platform_cut + BOND);
    assert_eq!(
        s.client.get_order(&order.order_id).status,
        OrderStatus::Released
    );
}

// ---------------------------------------------------------------------------
// Dispute path
// ---------------------------------------------------------------------------

#[test]
fn test_dispute_then_admin_resolves_partial_refund() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);

    // Customer disputes within the confirmation window.
    s.client.dispute(&s.customer, &order.order_id);

    // Evidence window must pass before admin rules.
    advance(&s.env, 48 * HOUR + 1);

    // Admin partial refund: 60% to customer, 30% to rider (keeps bond), 10% platform.
    let pot = FEE + BOND; // 1500
    let to_customer = 600i128;
    let to_rider = 750i128; // bond + small payment
    let to_platform = 150i128;
    let forfeit = pot - to_customer - to_rider - to_platform;
    let alloc = Allocation {
        to_customer,
        to_rider,
        to_platform,
        forfeit,
    };
    let result = s.client.resolve_dispute(&s.admin, &order.order_id, &alloc);
    assert_eq!(result.to_customer, to_customer);

    assert_eq!(
        s.token.balance(&s.customer),
        INITIAL_BALANCE - FEE + to_customer
    );
    assert_eq!(s.token.balance(&s.rider), INITIAL_BALANCE - BOND + to_rider);
    assert_eq!(s.token.balance(&s.platform_wallet), to_platform);

    assert_eq!(
        s.client.get_order(&order.order_id).status,
        OrderStatus::Resolved
    );
}

// ---------------------------------------------------------------------------
// Negative tests — auth and state guards
// ---------------------------------------------------------------------------

#[test]
fn test_mark_delivered_by_non_rider_fails() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);

    let stranger = Address::generate(&s.env);
    let result = s.client.try_mark_delivered(&stranger, &order.order_id);
    assert!(result.is_err());
}

#[test]
fn test_claim_before_window_elapses_fails() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);

    // Only 1 hour elapsed — window is 24h.
    advance(&s.env, HOUR);
    let result = s.client.try_claim_after_timeout(&s.rider, &order.order_id);
    assert!(result.is_err());
}

#[test]
fn test_dispute_after_window_closes_fails() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);

    advance(&s.env, 24 * HOUR + 1);
    let result = s.client.try_dispute(&s.customer, &order.order_id);
    assert!(result.is_err());
}

#[test]
fn test_resolve_before_evidence_window_fails() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);
    s.client.dispute(&s.customer, &order.order_id);

    advance(&s.env, HOUR); // Evidence window is 48h.
    let alloc = Allocation {
        to_customer: FEE,
        to_rider: BOND,
        to_platform: 0,
        forfeit: 0,
    };
    let result = s
        .client
        .try_resolve_dispute(&s.admin, &order.order_id, &alloc);
    assert!(result.is_err());
}

#[test]
fn test_resolve_allocation_sum_mismatch_fails() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);
    s.client.mark_delivered(&s.rider, &order.order_id);
    s.client.dispute(&s.customer, &order.order_id);
    advance(&s.env, 48 * HOUR + 1);

    // Sum is 100 too low.
    let bad = Allocation {
        to_customer: 500,
        to_rider: 500,
        to_platform: 400,
        forfeit: 0,
    };
    let result = s
        .client
        .try_resolve_dispute(&s.admin, &order.order_id, &bad);
    assert!(result.is_err());
}

#[test]
fn test_self_deal_blocked() {
    let s = setup();
    let result = s.client.try_create_order(
        &s.customer,
        &s.customer, // same as rider
        &FEE,
        &BOND,
        &PLATFORM_BPS,
        &(24 * HOUR),
        &(48 * HOUR),
        &String::from_str(&s.env, ""),
    );
    assert!(result.is_err());
}

#[test]
fn test_mutual_cancel_refunds_both_parties() {
    let s = setup();
    let order = create_default(&s);
    approve_both_parties(&s);
    s.client.fund(&order.order_id);

    s.client.mutual_cancel(&order.order_id);

    assert_eq!(s.token.balance(&s.customer), INITIAL_BALANCE);
    assert_eq!(s.token.balance(&s.rider), INITIAL_BALANCE);
    assert_eq!(s.token.balance(&s.contract_addr), 0);
    assert_eq!(
        s.client.get_order(&order.order_id).status,
        OrderStatus::Cancelled
    );
}
