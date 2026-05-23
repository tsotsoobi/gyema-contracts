#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token::TokenClient, Address,
    BytesN, Env, String, Vec,
};

// ---------------------------------------------------------------------------
// TTL constants (~5 s per ledger)
// ---------------------------------------------------------------------------
const INSTANCE_TTL_THRESHOLD: u32 = 17_280; // ~1 day
const INSTANCE_TTL_EXTEND: u32 = 518_400; // ~30 days
const PERSISTENT_TTL_THRESHOLD: u32 = 17_280;
const PERSISTENT_TTL_EXTEND_MIN: u32 = 518_400; // ~30 days floor
const SECS_PER_LEDGER: u64 = 5;

// ---------------------------------------------------------------------------
// Protocol parameters
// ---------------------------------------------------------------------------
/// Default window (seconds) after `mark_delivered` during which the customer
/// can confirm or dispute. After this window the rider may claim funds.
const DEFAULT_CONFIRMATION_WINDOW_SECS: u64 = 86_400; // 24 hours

/// Default window (seconds) after `dispute` during which both parties can
/// submit evidence before the admin may rule.
const DEFAULT_EVIDENCE_WINDOW_SECS: u64 = 172_800; // 48 hours

/// Maximum allowed confirmation window (caps merchant/admin configuration).
const MAX_CONFIRMATION_WINDOW_SECS: u64 = 7 * 86_400; // 7 days

/// Maximum allowed evidence window (caps admin configuration).
const MAX_EVIDENCE_WINDOW_SECS: u64 = 14 * 86_400; // 14 days

/// Basis-point denominator for platform fee math.
const BPS_DENOMINATOR: i128 = 10_000;

/// Maximum platform fee in basis points (50% sanity cap).
const MAX_PLATFORM_FEE_BPS: u32 = 5_000;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    InvalidAmount = 1,
    InvalidBond = 2,
    InvalidFeeBps = 3,
    InvalidWindow = 4,
    InvalidStatus = 5,
    OrderNotFound = 6,
    Unauthorized = 7,
    NotInOpenState = 8,
    NotInFundedState = 9,
    NotInDeliveredState = 10,
    NotInDisputedState = 11,
    ConfirmationWindowOpen = 12,
    ConfirmationWindowClosed = 13,
    EvidenceWindowOpen = 14,
    AllocationExceedsPot = 15,
    TimestampOverflow = 16,
    SelfDealNotAllowed = 17,
    AlreadyFunded = 18,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------
#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    // Instance storage
    Admin,
    Token,
    PlatformFeeWallet,
    NextOrderId,
    // Persistent storage
    Order(u64),
    CustomerOrders(Address),
    RiderOrders(Address),
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Lifecycle of a single delivery order.
///
/// ```
/// Open --(fund)--> Funded --(mark_delivered)--> Delivered
///                                                  |
///                              +-------------------+--------------------+
///                              | (confirm)         | (dispute)          | (claim_after_timeout)
///                              v                   v                    v
///                           Released            Disputed             Released
///                                                  |
///                                              (resolve)
///                                                  v
///                                              Resolved
///
/// Open / Funded --(mutual_cancel)--> Cancelled
/// ```
#[derive(Clone, Copy, PartialEq, Debug)]
#[contracttype]
#[repr(u32)]
pub enum OrderStatus {
    Open = 0,
    Funded = 1,
    Delivered = 2,
    Disputed = 3,
    Released = 4,
    Resolved = 5,
    Cancelled = 6,
}

/// A single delivery escrow record. All monetary fields are denominated in the
/// platform token (e.g. native Pi via the Stellar Asset Contract).
#[derive(Clone, PartialEq, Debug)]
#[contracttype]
pub struct Order {
    pub order_id: u64,
    pub customer: Address,
    pub rider: Address,
    /// Delivery fee paid by the customer (held in escrow).
    pub delivery_fee: i128,
    /// Bond staked by the rider when they accept the order (held in escrow).
    pub rider_bond: i128,
    /// Platform fee, deducted from `delivery_fee` on release. In basis points.
    pub platform_fee_bps: u32,
    /// Confirmation window (seconds) after `mark_delivered`.
    pub confirmation_window_secs: u64,
    /// Evidence window (seconds) after `dispute`.
    pub evidence_window_secs: u64,
    pub status: OrderStatus,
    pub created_at: u64,
    /// Ledger timestamp when `mark_delivered` was called. 0 if not yet delivered.
    pub delivered_at: u64,
    /// Ledger timestamp when `dispute` was called. 0 if not disputed.
    pub disputed_at: u64,
    /// Optional metadata pointer (e.g. IPFS hash of order details, route, photos).
    pub metadata_uri: String,
}

/// Resolution payout instruction supplied by the admin when ruling a dispute.
///
/// All four amounts must sum to exactly `delivery_fee + rider_bond`.
/// (Platform fee is allocated explicitly here too, so the admin has full
/// discretion to waive it when refunding.)
#[derive(Clone, PartialEq, Debug)]
#[contracttype]
pub struct Allocation {
    pub to_customer: i128,
    pub to_rider: i128,
    pub to_platform: i128,
    /// Burn / forfeit (kept by the contract or rolled to platform). Use 0
    /// in normal cases; non-zero only for slashing scenarios where the admin
    /// wants explicit accounting separation.
    pub forfeit: i128,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------
#[contract]
pub struct DeliveryEscrowContract;

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------
fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND);
}

/// Compute TTL extend: max(window_secs * 2 / SECS_PER_LEDGER, MIN_FLOOR).
/// For escrow we extend based on the longest possible active window
/// (confirmation + evidence) so an order's data survives until resolution.
fn ttl_extend_for_window(secs: u64) -> u32 {
    let ledgers = secs.saturating_mul(2) / SECS_PER_LEDGER;
    let capped = if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    };
    core::cmp::max(capped, PERSISTENT_TTL_EXTEND_MIN)
}

fn bump_persistent(env: &Env, key: &DataKey, life_secs: u64) {
    env.storage().persistent().extend_ttl(
        key,
        PERSISTENT_TTL_THRESHOLD,
        ttl_extend_for_window(life_secs),
    );
}

fn next_order_id(env: &Env) -> u64 {
    let id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::NextOrderId)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&DataKey::NextOrderId, &(id + 1));
    id
}

fn checked_add_ts(a: u64, b: u64) -> Result<u64, ContractError> {
    a.checked_add(b).ok_or(ContractError::TimestampOverflow)
}

fn get_token(env: &Env) -> Address {
    env.storage().instance().get(&DataKey::Token).unwrap()
}

fn get_admin(env: &Env) -> Address {
    env.storage().instance().get(&DataKey::Admin).unwrap()
}

fn get_platform_wallet(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::PlatformFeeWallet)
        .unwrap()
}

fn load_order(env: &Env, order_id: u64) -> Result<Order, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Order(order_id))
        .ok_or(ContractError::OrderNotFound)
}

fn save_order(env: &Env, order: &Order) {
    let key = DataKey::Order(order.order_id);
    env.storage().persistent().set(&key, order);
    let life = order
        .confirmation_window_secs
        .saturating_add(order.evidence_window_secs);
    bump_persistent(env, &key, life);
}

fn append_to_index(env: &Env, key: &DataKey, order_id: u64, life_secs: u64) {
    let mut ids: Vec<u64> = env
        .storage()
        .persistent()
        .get(key)
        .unwrap_or_else(|| Vec::new(env));
    ids.push_back(order_id);
    env.storage().persistent().set(key, &ids);
    bump_persistent(env, key, life_secs);
}

fn require_status(order: &Order, expected: OrderStatus) -> Result<(), ContractError> {
    if order.status == expected {
        return Ok(());
    }
    Err(match expected {
        OrderStatus::Open => ContractError::NotInOpenState,
        OrderStatus::Funded => ContractError::NotInFundedState,
        OrderStatus::Delivered => ContractError::NotInDeliveredState,
        OrderStatus::Disputed => ContractError::NotInDisputedState,
        _ => ContractError::InvalidStatus,
    })
}

/// Validate that an Allocation sums to the available pot and contains no
/// negative components.
fn validate_allocation(a: &Allocation, pot: i128) -> Result<(), ContractError> {
    if a.to_customer < 0 || a.to_rider < 0 || a.to_platform < 0 || a.forfeit < 0 {
        return Err(ContractError::InvalidAmount);
    }
    let sum = a
        .to_customer
        .checked_add(a.to_rider)
        .and_then(|x| x.checked_add(a.to_platform))
        .and_then(|x| x.checked_add(a.forfeit))
        .ok_or(ContractError::TimestampOverflow)?;
    if sum != pot {
        return Err(ContractError::AllocationExceedsPot);
    }
    Ok(())
}

/// Standard happy-path payout: rider gets delivery_fee minus platform fee,
/// plus their bond back; platform wallet gets the fee.
fn standard_payout(env: &Env, order: &Order) -> Result<Allocation, ContractError> {
    let platform_cut = order
        .delivery_fee
        .checked_mul(order.platform_fee_bps as i128)
        .and_then(|x| Some(x / BPS_DENOMINATOR))
        .ok_or(ContractError::TimestampOverflow)?;
    let rider_take = order
        .delivery_fee
        .checked_sub(platform_cut)
        .ok_or(ContractError::InvalidAmount)?
        .checked_add(order.rider_bond)
        .ok_or(ContractError::TimestampOverflow)?;
    let _ = env; // silence warning in non-test builds
    Ok(Allocation {
        to_customer: 0,
        to_rider: rider_take,
        to_platform: platform_cut,
        forfeit: 0,
    })
}

/// Execute an Allocation by transferring from this contract to the named parties.
fn execute_payout(env: &Env, order: &Order, a: &Allocation) {
    let token = get_token(env);
    let token_client = TokenClient::new(env, &token);
    let contract_addr = env.current_contract_address();

    if a.to_customer > 0 {
        token_client.transfer(&contract_addr, &order.customer, &a.to_customer);
    }
    if a.to_rider > 0 {
        token_client.transfer(&contract_addr, &order.rider, &a.to_rider);
    }
    if a.to_platform > 0 {
        let pw = get_platform_wallet(env);
        token_client.transfer(&contract_addr, &pw, &a.to_platform);
    }
    // forfeit stays in the contract balance; if non-zero, surface it via event.
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------
#[contractimpl]
impl DeliveryEscrowContract {
    // ---- Constructor ------------------------------------------------------

    pub fn __constructor(
        env: Env,
        admin: Address,
        token: Address,
        platform_fee_wallet: Address,
    ) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeWallet, &platform_fee_wallet);
        env.storage().instance().set(&DataKey::NextOrderId, &0u64);
        bump_instance(&env);
    }

    // ---- Order lifecycle --------------------------------------------------

    /// Create a new escrow order. The customer specifies the rider, delivery
    /// fee, required bond, and platform parameters. The order starts in
    /// `Open` state — no funds have moved yet. The customer must then call
    /// `fund` and the rider must call `stake_bond` to advance to `Funded`.
    ///
    /// We separate `create` from `fund` so the customer can review the rider's
    /// acceptance before parting with funds, and so a frontend can show the
    /// full agreed terms before any token transfer.
    pub fn create_order(
        env: Env,
        customer: Address,
        rider: Address,
        delivery_fee: i128,
        rider_bond: i128,
        platform_fee_bps: u32,
        confirmation_window_secs: u64,
        evidence_window_secs: u64,
        metadata_uri: String,
    ) -> Result<Order, ContractError> {
        if delivery_fee <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        if rider_bond < 0 {
            return Err(ContractError::InvalidBond);
        }
        if platform_fee_bps > MAX_PLATFORM_FEE_BPS {
            return Err(ContractError::InvalidFeeBps);
        }
        if confirmation_window_secs == 0
            || confirmation_window_secs > MAX_CONFIRMATION_WINDOW_SECS
        {
            return Err(ContractError::InvalidWindow);
        }
        if evidence_window_secs == 0 || evidence_window_secs > MAX_EVIDENCE_WINDOW_SECS {
            return Err(ContractError::InvalidWindow);
        }
        if customer == rider {
            return Err(ContractError::SelfDealNotAllowed);
        }

        customer.require_auth();

        let order_id = next_order_id(&env);
        let now = env.ledger().timestamp();

        let order = Order {
            order_id,
            customer: customer.clone(),
            rider: rider.clone(),
            delivery_fee,
            rider_bond,
            platform_fee_bps,
            confirmation_window_secs,
            evidence_window_secs,
            status: OrderStatus::Open,
            created_at: now,
            delivered_at: 0,
            disputed_at: 0,
            metadata_uri,
        };

        save_order(&env, &order);
        let life = confirmation_window_secs.saturating_add(evidence_window_secs);
        append_to_index(&env, &DataKey::CustomerOrders(customer), order_id, life);
        append_to_index(&env, &DataKey::RiderOrders(rider), order_id, life);
        bump_instance(&env);

        env.events()
            .publish((symbol_short!("ord_new"),), order.clone());

        Ok(order)
    }

    /// Customer funds the delivery fee into the contract, AND the rider
    /// simultaneously stakes their bond. We require both transfers in a
    /// single call so the order atomically advances from `Open` to `Funded`.
    /// Both parties must have pre-approved the contract for their respective
    /// amounts via `token.approve()`.
    ///
    /// Combining funding and staking into one transaction prevents the
    /// griefing case where a customer funds and the rider never stakes
    /// (leaving customer funds locked).
    pub fn fund(env: Env, order_id: u64) -> Result<Order, ContractError> {
        let mut order = load_order(&env, order_id)?;
        require_status(&order, OrderStatus::Open)?;

        // Both parties must consent — customer's funds + rider's bond move now.
        order.customer.require_auth();
        order.rider.require_auth();

        let token = get_token(&env);
        let token_client = TokenClient::new(&env, &token);
        let contract_addr = env.current_contract_address();

        // Pull the customer's delivery fee.
        token_client.transfer_from(
            &contract_addr,
            &order.customer,
            &contract_addr,
            &order.delivery_fee,
        );

        // Pull the rider's bond (if any).
        if order.rider_bond > 0 {
            token_client.transfer_from(
                &contract_addr,
                &order.rider,
                &contract_addr,
                &order.rider_bond,
            );
        }

        order.status = OrderStatus::Funded;
        save_order(&env, &order);
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("ord_fund"),),
            (order.order_id, order.delivery_fee, order.rider_bond),
        );

        Ok(order)
    }

    /// Rider marks the order as delivered. Starts the confirmation window
    /// during which the customer can confirm or dispute. If the customer
    /// does neither before the window closes, the rider can call
    /// `claim_after_timeout` to receive payout.
    pub fn mark_delivered(
        env: Env,
        rider: Address,
        order_id: u64,
    ) -> Result<Order, ContractError> {
        let mut order = load_order(&env, order_id)?;
        require_status(&order, OrderStatus::Funded)?;

        if order.rider != rider {
            return Err(ContractError::Unauthorized);
        }
        rider.require_auth();

        let now = env.ledger().timestamp();
        order.status = OrderStatus::Delivered;
        order.delivered_at = now;
        save_order(&env, &order);
        bump_instance(&env);

        env.events()
            .publish((symbol_short!("delivrd"),), (order.order_id, now));

        Ok(order)
    }

    /// Customer confirms receipt. Releases funds per the standard payout:
    /// rider receives `delivery_fee - platform_cut + rider_bond`,
    /// platform wallet receives `platform_cut`.
    ///
    /// This is the happy-path terminal state. We make customer-confirms the
    /// primary release because the customer is the only party who can verify
    /// "I received the right item." A rider-only confirm flow is a known
    /// fraud vector.
    pub fn confirm_delivery(
        env: Env,
        customer: Address,
        order_id: u64,
    ) -> Result<Allocation, ContractError> {
        let mut order = load_order(&env, order_id)?;
        require_status(&order, OrderStatus::Delivered)?;

        if order.customer != customer {
            return Err(ContractError::Unauthorized);
        }
        customer.require_auth();

        let allocation = standard_payout(&env, &order)?;
        execute_payout(&env, &order, &allocation);

        order.status = OrderStatus::Released;
        save_order(&env, &order);
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("released"),),
            (order.order_id, allocation.clone()),
        );

        Ok(allocation)
    }

    /// Rider claims payout after the confirmation window has expired with
    /// no customer response (silence = consent). Same payout as
    /// `confirm_delivery`. Prevents customer-side griefing where the
    /// customer simply never confirms.
    pub fn claim_after_timeout(
        env: Env,
        rider: Address,
        order_id: u64,
    ) -> Result<Allocation, ContractError> {
        let mut order = load_order(&env, order_id)?;
        require_status(&order, OrderStatus::Delivered)?;

        if order.rider != rider {
            return Err(ContractError::Unauthorized);
        }
        rider.require_auth();

        let now = env.ledger().timestamp();
        let window_end = checked_add_ts(order.delivered_at, order.confirmation_window_secs)?;
        if now < window_end {
            return Err(ContractError::ConfirmationWindowOpen);
        }

        let allocation = standard_payout(&env, &order)?;
        execute_payout(&env, &order, &allocation);

        order.status = OrderStatus::Released;
        save_order(&env, &order);
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("released"),),
            (order.order_id, allocation.clone()),
        );

        Ok(allocation)
    }

    // ---- Dispute path -----------------------------------------------------

    /// Either party (customer OR rider) opens a dispute during the
    /// confirmation window. Freezes the escrow into `Disputed` state and
    /// starts the evidence window.
    ///
    /// We allow either side to dispute because:
    ///   - Customer disputes when goods are wrong/missing/damaged.
    ///   - Rider may need to dispute when, e.g., the customer is refusing
    ///     delivery or the address turned out to be fraudulent.
    pub fn dispute(
        env: Env,
        caller: Address,
        order_id: u64,
    ) -> Result<Order, ContractError> {
        let mut order = load_order(&env, order_id)?;
        require_status(&order, OrderStatus::Delivered)?;

        if caller != order.customer && caller != order.rider {
            return Err(ContractError::Unauthorized);
        }
        caller.require_auth();

        let now = env.ledger().timestamp();
        let window_end = checked_add_ts(order.delivered_at, order.confirmation_window_secs)?;
        if now >= window_end {
            return Err(ContractError::ConfirmationWindowClosed);
        }

        order.status = OrderStatus::Disputed;
        order.disputed_at = now;
        save_order(&env, &order);
        bump_instance(&env);

        env.events()
            .publish((symbol_short!("disputed"),), (order.order_id, caller, now));

        Ok(order)
    }

    /// Admin resolves a dispute with an explicit `Allocation`. The allocation
    /// must sum exactly to the available pot (delivery_fee + rider_bond)
    /// and contain no negative amounts. This explicit-sum check forces the
    /// admin to do the accounting on their end and surfaces mistakes early.
    ///
    /// The evidence window must have elapsed before the admin may rule —
    /// this protects both parties' right to submit evidence.
    pub fn resolve_dispute(
        env: Env,
        admin: Address,
        order_id: u64,
        allocation: Allocation,
    ) -> Result<Allocation, ContractError> {
        let mut order = load_order(&env, order_id)?;
        require_status(&order, OrderStatus::Disputed)?;

        let stored_admin = get_admin(&env);
        if admin != stored_admin {
            return Err(ContractError::Unauthorized);
        }
        admin.require_auth();

        let now = env.ledger().timestamp();
        let evidence_end = checked_add_ts(order.disputed_at, order.evidence_window_secs)?;
        if now < evidence_end {
            return Err(ContractError::EvidenceWindowOpen);
        }

        let pot = order
            .delivery_fee
            .checked_add(order.rider_bond)
            .ok_or(ContractError::TimestampOverflow)?;
        validate_allocation(&allocation, pot)?;

        execute_payout(&env, &order, &allocation);

        order.status = OrderStatus::Resolved;
        save_order(&env, &order);
        bump_instance(&env);

        env.events().publish(
            (symbol_short!("resolved"),),
            (order.order_id, allocation.clone()),
        );

        Ok(allocation)
    }

    // ---- Mutual cancellation ---------------------------------------------

    /// Both parties mutually cancel a Funded order (e.g. customer changed
    /// their mind, rider can't make the trip). Full refund: customer gets
    /// delivery_fee back, rider gets bond back, no platform fee charged.
    ///
    /// Requires both parties' auth. Only allowed before `mark_delivered`.
    pub fn mutual_cancel(env: Env, order_id: u64) -> Result<Order, ContractError> {
        let mut order = load_order(&env, order_id)?;
        if order.status != OrderStatus::Funded && order.status != OrderStatus::Open {
            return Err(ContractError::InvalidStatus);
        }
        order.customer.require_auth();
        order.rider.require_auth();

        if order.status == OrderStatus::Funded {
            let token = get_token(&env);
            let token_client = TokenClient::new(&env, &token);
            let contract_addr = env.current_contract_address();
            if order.delivery_fee > 0 {
                token_client.transfer(&contract_addr, &order.customer, &order.delivery_fee);
            }
            if order.rider_bond > 0 {
                token_client.transfer(&contract_addr, &order.rider, &order.rider_bond);
            }
        }

        order.status = OrderStatus::Cancelled;
        save_order(&env, &order);
        bump_instance(&env);

        env.events()
            .publish((symbol_short!("cancel"),), order.order_id);

        Ok(order)
    }

    // ---- Query functions --------------------------------------------------

    pub fn get_order(env: Env, order_id: u64) -> Result<Order, ContractError> {
        load_order(&env, order_id)
    }

    pub fn get_customer_orders(env: Env, customer: Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::CustomerOrders(customer))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_rider_orders(env: Env, rider: Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::RiderOrders(rider))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// True if the order is in Delivered state and the confirmation window
    /// has elapsed (i.e. rider may call `claim_after_timeout`).
    pub fn is_timeout_claimable(env: Env, order_id: u64) -> bool {
        let order = match load_order(&env, order_id) {
            Ok(o) => o,
            Err(_) => return false,
        };
        if order.status != OrderStatus::Delivered {
            return false;
        }
        let now = env.ledger().timestamp();
        let window_end = match order
            .delivered_at
            .checked_add(order.confirmation_window_secs)
        {
            Some(v) => v,
            None => return false,
        };
        now >= window_end
    }

    // ---- Admin functions --------------------------------------------------

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin = get_admin(&env);
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        env.events()
            .publish((symbol_short!("upgrade"),), new_wasm_hash);
    }

    pub fn version(_env: Env) -> u32 {
        1
    }
}

#[cfg(test)]
mod test;
