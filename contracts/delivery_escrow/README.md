# Delivery Escrow Smart Contract

A Soroban smart contract for two-sided delivery escrow on the Pi Network. Customers fund delivery fees and riders stake performance bonds; funds release on customer confirmation, on rider timeout claim if the customer is silent, or via admin-arbitrated dispute resolution.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Data Model](#data-model)
- [Contract API](#contract-api)
- [Release Flow](#release-flow)
- [Dispute Flow](#dispute-flow)
- [Access Control](#access-control)
- [Events](#events)
- [Error Codes](#error-codes)
- [Storage Layout & TTL](#storage-layout--ttl)
- [Security Notes](#security-notes)

---

## Overview

The contract implements a three-pot escrow with a confirmation window and admin-arbitrated disputes:

1. **Customer** creates an order naming the rider, delivery fee, required bond, and window parameters.
2. **Customer and rider both call `fund`** in a single transaction — customer's delivery fee and rider's bond move into the contract atomically.
3. **Rider** calls `mark_delivered` when the delivery is complete, opening a confirmation window.
4. Within the confirmation window, the **customer** can either `confirm_delivery` (standard payout) or `dispute` (freeze the escrow into the evidence window).
5. If the customer is silent past the confirmation window, the **rider** can `claim_after_timeout` to receive the standard payout.
6. If disputed, the **admin** rules via `resolve_dispute` after the evidence window elapses, supplying an explicit `Allocation` that splits the pot between customer, rider, platform, and optional forfeit.

Key design properties:
- **Customer-confirms primary** — the customer is the only party who can verify "I received the right item." Rider self-confirmation is a known fraud vector and is not offered.
- **Timeout escape valve** — rider can claim after the window so customer silence isn't a permanent grief vector.
- **Either side can dispute** — riders need dispute access too (fraudulent address, refused delivery, customer chargeback risk).
- **Explicit allocation in disputes** — admin must supply amounts that sum to exactly the pot. Forces accounting discipline and surfaces mistakes early.
- **Mutual cancel** — both parties can voluntarily unwind a Funded order before `mark_delivered` for a clean full refund.
- **No partial fills** — an order is one delivery. No batching, no splits at the order layer.
- **No self-dealing** — customer and rider must be distinct addresses.

---

## Architecture

```
Customer                  Contract                  Token Contract             Rider                 Admin
   |                         |                            |                       |                     |
   |-- create_order ------->|                            |                       |                     |
   |   (Open)               |                            |                       |                     |
   |                         |                            |                       |                     |
   |--- approve(contract,fee)-------------------------> |                       |                     |
   |                                                      |<-- approve(bond) ---|                     |
   |                         |                            |                       |                     |
   |-- fund ---------------> |-- transfer_from(fee) ---> |                       |                     |
   |                         |-- transfer_from(bond) --> | <---------------------|                     |
   |   (Funded)              |                            |                       |                     |
   |                         |                            |                       |                     |
   |                         |<------- mark_delivered ---------------------------|                     |
   |   (Delivered, T0)       |                            |                       |                     |
   |                         |                            |                       |                     |
   |-- confirm ------------> |-- transfer(payout) ------> | ---> rider, platform |                     |
   |   (Released)            |                            |                       |                     |
   |                         |                            |                       |                     |
   |                         |    ...or, after window:                                                  |
   |                         |<------- claim_after_timeout (T0 + conf_window) -----                    |
   |                         |-- transfer(payout) ------> | ---> rider, platform |                     |
   |                         |                            |                       |                     |
   |   ...or, either side disputes within window:                                                       |
   |-- dispute ------------> |   (Disputed, T1)           |                       |                     |
   |                         |                            |                       |                     |
   |                         |<----------- resolve_dispute (T1 + evidence_window) --------------------|
   |                         |-- transfer(allocation) -->|---> all named parties                       |
   |   (Resolved)            |                            |                       |                     |
```

---

## Data Model

### Order

The single core record. One per delivery.

| Field                       | Type          | Description                                                                              |
|-----------------------------|---------------|------------------------------------------------------------------------------------------|
| `order_id`                  | `u64`         | Auto-incrementing unique identifier                                                      |
| `customer`                  | `Address`     | Address that funds the delivery fee                                                      |
| `rider`                     | `Address`     | Address that stakes the bond and performs the delivery                                   |
| `delivery_fee`              | `i128`        | Amount the customer pays (held in escrow until release)                                  |
| `rider_bond`                | `i128`        | Performance bond the rider stakes (held in escrow; may be 0)                             |
| `platform_fee_bps`          | `u32`         | Platform fee in basis points, deducted from `delivery_fee` on standard release           |
| `confirmation_window_secs`  | `u64`         | Seconds after `mark_delivered` during which customer may confirm/dispute                 |
| `evidence_window_secs`      | `u64`         | Seconds after `dispute` during which evidence may be submitted off-chain before ruling   |
| `status`                    | `OrderStatus` | Current lifecycle state                                                                  |
| `created_at`                | `u64`         | Ledger timestamp at creation                                                             |
| `delivered_at`              | `u64`         | Ledger timestamp when `mark_delivered` was called (0 if not yet)                         |
| `disputed_at`               | `u64`         | Ledger timestamp when `dispute` was called (0 if not disputed)                           |
| `metadata_uri`              | `String`      | Off-chain pointer (e.g. IPFS hash) for delivery details, route, photos                   |

### OrderStatus

```
Open      -> created, no funds moved yet
Funded    -> both fee and bond in escrow
Delivered -> rider marked complete, confirmation window open
Disputed  -> dispute opened, evidence window open
Released  -> standard happy-path payout completed (terminal)
Resolved  -> admin-arbitrated payout completed (terminal)
Cancelled -> mutual cancellation, full refund (terminal)
```

### Allocation

Supplied by the admin when calling `resolve_dispute`. Must sum to exactly `delivery_fee + rider_bond` and contain no negatives.

| Field         | Type   | Description                                                              |
|---------------|--------|--------------------------------------------------------------------------|
| `to_customer` | `i128` | Refund amount to the customer                                            |
| `to_rider`    | `i128` | Payout to the rider (often = bond return + partial payment)              |
| `to_platform` | `i128` | Platform fee. Admin may waive (set to 0) in refund scenarios.            |
| `forfeit`     | `i128` | Slash amount, retained by the contract for explicit accounting separation|

---

## Contract API

### Constructor

```rust
fn __constructor(env: Env, admin: Address, token: Address, platform_fee_wallet: Address)
```

Initializes the contract. Sets admin, token, platform fee wallet, and zeroes the order ID counter. Called once at deployment.

---

### Order Lifecycle

#### `create_order`

```rust
fn create_order(
    env: Env,
    customer: Address,
    rider: Address,
    delivery_fee: i128,
    rider_bond: i128,
    platform_fee_bps: u32,
    confirmation_window_secs: u64,
    evidence_window_secs: u64,
    metadata_uri: String,
) -> Result<Order, ContractError>
```

Creates a new order in `Open` state. No funds move yet.

**Auth:** `customer`

**Validation:**
- `delivery_fee > 0` -> `InvalidAmount`
- `rider_bond >= 0` -> `InvalidBond`
- `platform_fee_bps <= 5_000` (50% cap) -> `InvalidFeeBps`
- `0 < confirmation_window_secs <= 7 days` -> `InvalidWindow`
- `0 < evidence_window_secs <= 14 days` -> `InvalidWindow`
- `customer != rider` -> `SelfDealNotAllowed`

**Effects:**
- Stores `Order` under `DataKey::Order(order_id)`
- Appends `order_id` to `DataKey::CustomerOrders(customer)` and `DataKey::RiderOrders(rider)`
- Emits `ord_new` event

---

#### `fund`

```rust
fn fund(env: Env, order_id: u64) -> Result<Order, ContractError>
```

Atomically pulls the delivery fee from the customer and the bond from the rider into the contract. Advances state from `Open` to `Funded`.

Both parties must have pre-approved the contract via `token.approve()`.

**Auth:** `customer` AND `rider` (both must sign).

**Validation:**
- Status must be `Open` -> `NotInOpenState`

**Effects:**
- `token.transfer_from(customer -> contract, delivery_fee)`
- `token.transfer_from(rider -> contract, rider_bond)` (skipped if bond is 0)
- Sets `status = Funded`
- Emits `ord_fund` event

**Rationale:** Combining funding and staking into one call prevents the asymmetric grief case where the customer funds and the rider never stakes, locking customer funds.

---

#### `mark_delivered`

```rust
fn mark_delivered(env: Env, rider: Address, order_id: u64) -> Result<Order, ContractError>
```

Rider signals delivery is complete. Records `delivered_at` and opens the confirmation window.

**Auth:** `rider` (must own the order)

**Validation:**
- Status must be `Funded` -> `NotInFundedState`

**Effects:**
- Sets `status = Delivered`, `delivered_at = now`
- Emits `delivrd` event

---

#### `confirm_delivery`

```rust
fn confirm_delivery(
    env: Env,
    customer: Address,
    order_id: u64,
) -> Result<Allocation, ContractError>
```

Customer acknowledges receipt. Releases the standard payout:
- Rider receives `delivery_fee - platform_cut + rider_bond`
- Platform wallet receives `platform_cut = delivery_fee * platform_fee_bps / 10_000`
- Customer receives nothing (they got the goods)

**Auth:** `customer` (must own the order)

**Validation:**
- Status must be `Delivered` -> `NotInDeliveredState`

**Effects:**
- Executes payout via `token.transfer` from contract to rider and platform wallet
- Sets `status = Released`
- Emits `released` event

---

#### `claim_after_timeout`

```rust
fn claim_after_timeout(
    env: Env,
    rider: Address,
    order_id: u64,
) -> Result<Allocation, ContractError>
```

After the confirmation window elapses with no customer confirm or dispute, the rider may claim the standard payout. Treats customer silence as consent.

**Auth:** `rider` (must own the order)

**Validation:**
- Status must be `Delivered` -> `NotInDeliveredState`
- `now >= delivered_at + confirmation_window_secs` -> else `ConfirmationWindowOpen`

**Effects:** Same payout and state changes as `confirm_delivery`.

---

### Dispute Path

#### `dispute`

```rust
fn dispute(env: Env, caller: Address, order_id: u64) -> Result<Order, ContractError>
```

Either party (customer or rider) freezes the escrow within the confirmation window. Opens the evidence window.

**Auth:** `caller` must be the customer or the rider on the order.

**Validation:**
- Status must be `Delivered` -> `NotInDeliveredState`
- `now < delivered_at + confirmation_window_secs` -> else `ConfirmationWindowClosed`

**Effects:**
- Sets `status = Disputed`, `disputed_at = now`
- Emits `disputed` event with the disputing party's address

---

#### `resolve_dispute`

```rust
fn resolve_dispute(
    env: Env,
    admin: Address,
    order_id: u64,
    allocation: Allocation,
) -> Result<Allocation, ContractError>
```

Admin rules on the dispute after the evidence window. Supplies an explicit allocation that must sum to exactly `delivery_fee + rider_bond`.

**Auth:** Caller must be the contract `admin` set at construction.

**Validation:**
- Status must be `Disputed` -> `NotInDisputedState`
- `now >= disputed_at + evidence_window_secs` -> else `EvidenceWindowOpen`
- Allocation components all non-negative -> else `InvalidAmount`
- Allocation components sum to pot -> else `AllocationExceedsPot`

**Effects:**
- Executes payout per the allocation
- Sets `status = Resolved`
- Emits `resolved` event with the full allocation

---

### Mutual Cancellation

#### `mutual_cancel`

```rust
fn mutual_cancel(env: Env, order_id: u64) -> Result<Order, ContractError>
```

Both parties voluntarily cancel a Funded (or Open) order. Full refund: customer gets `delivery_fee` back, rider gets `rider_bond` back, no platform fee charged.

**Auth:** `customer` AND `rider`.

**Validation:** Status must be `Open` or `Funded` -> else `InvalidStatus`.

**Effects:**
- If Funded: refunds both parties via `token.transfer`
- Sets `status = Cancelled`
- Emits `cancel` event

---

### Query Functions

All query functions are read-only and do not bump TTLs.

| Function                 | Returns                       | Auth     |
|--------------------------|-------------------------------|----------|
| `get_order(id)`          | `Order`                       | None     |
| `get_customer_orders(a)` | `Vec<u64>` of order IDs       | None     |
| `get_rider_orders(a)`    | `Vec<u64>` of order IDs       | None     |
| `is_timeout_claimable(id)` | `bool` — true if rider can claim now | None |

---

### Admin Functions

| Function                            | Description                  | Auth    |
|-------------------------------------|------------------------------|---------|
| `upgrade(new_wasm_hash)`            | Replace contract WASM        | `admin` |
| `version()`                         | Returns contract version (1) | None    |

---

## Release Flow

### Standard payout calculation

```
platform_cut = delivery_fee * platform_fee_bps / 10_000
rider_take   = delivery_fee - platform_cut + rider_bond
customer_take = 0
```

### Timeline example (24h confirmation, 48h evidence)

```
T+0    : create_order (Open)
T+5m   : approve x2, fund (Funded)
T+2h   : mark_delivered (Delivered, delivered_at = T+2h)
       |
       +-- Happy path A: customer calls confirm_delivery at T+3h -> Released
       |
       +-- Happy path B: customer silent. At T+26h+ rider calls claim_after_timeout -> Released
       |
       +-- Dispute path: customer calls dispute at T+10h (Disputed, disputed_at = T+10h)
                evidence window runs until T+58h
                admin calls resolve_dispute at T+58h+ with allocation -> Resolved
```

---

## Dispute Flow

The admin must construct an `Allocation` summing to `delivery_fee + rider_bond`. The contract validates this on chain — there is no "default" allocation. Some illustrative rulings:

| Scenario                                  | to_customer | to_rider           | to_platform   | forfeit |
|-------------------------------------------|-------------|--------------------|---------------|---------|
| Rider fully at fault (no delivery)        | `fee + bond`| 0                  | 0             | 0       |
| Rider partially at fault (late, damaged)  | `fee/2`     | `bond + fee/2 - p` | `p` (small)   | 0       |
| Customer at fault (refused delivery)      | 0           | `fee + bond - p`   | `p`           | 0       |
| Rider grossly at fault (slash bond)       | `fee`       | 0                  | 0             | `bond`  |
| Mutual fault, split losses                | `fee*0.4`   | `bond + fee*0.4`   | `fee*0.2`     | 0       |

---

## Access Control

| Function              | Requires Auth                       | Notes                                       |
|-----------------------|-------------------------------------|---------------------------------------------|
| `create_order`        | `customer`                          | Customer is the party committing fee        |
| `fund`                | `customer` AND `rider`              | Atomic two-sided commitment                 |
| `mark_delivered`      | `rider` (own order)                 | Only rider can declare delivery             |
| `confirm_delivery`    | `customer` (own order)              | Only customer can verify receipt            |
| `claim_after_timeout` | `rider` (own order)                 | After confirmation window expires           |
| `dispute`             | `customer` OR `rider` (own order)   | Either side, within confirmation window     |
| `resolve_dispute`     | `admin`                             | After evidence window expires               |
| `mutual_cancel`       | `customer` AND `rider`              | Before delivery; full refund                |
| `get_*`               | None                                | Public read                                 |
| `upgrade`             | `admin`                             | Standard upgrade path                       |
| `version`             | None                                | Public read                                 |

All authenticated functions use `require_auth()` and verify the caller matches the expected role. Mismatches return `Unauthorized`. State guards return state-specific errors (`NotInFundedState`, `ConfirmationWindowOpen`, etc.) to make failures debuggable.

---

## Events

| Symbol     | Description                          | Data                                          |
|------------|--------------------------------------|-----------------------------------------------|
| `ord_new`  | Order created                        | `Order` struct                                |
| `ord_fund` | Order funded by both parties         | `(order_id, delivery_fee, rider_bond)`        |
| `delivrd`  | Rider marked delivered               | `(order_id, delivered_at)`                    |
| `released` | Standard payout completed            | `(order_id, Allocation)`                      |
| `disputed` | Dispute opened                       | `(order_id, disputing_party, disputed_at)`    |
| `resolved` | Admin-arbitrated payout completed    | `(order_id, Allocation)`                      |
| `cancel`   | Order mutually cancelled             | `order_id`                                    |
| `upgrade`  | Contract WASM upgraded               | `new_wasm_hash`                               |

---

## Error Codes

| Code | Name                     | Meaning                                                              |
|------|--------------------------|----------------------------------------------------------------------|
| 1    | `InvalidAmount`          | A monetary amount was negative or zero where positive is required    |
| 2    | `InvalidBond`            | Bond was negative                                                    |
| 3    | `InvalidFeeBps`          | Platform fee BPS exceeds 5_000 (50%)                                 |
| 4    | `InvalidWindow`          | Window seconds out of allowed range                                  |
| 5    | `InvalidStatus`          | Operation not permitted in current status                            |
| 6    | `OrderNotFound`          | `order_id` does not exist in storage                                 |
| 7    | `Unauthorized`           | Caller is not the expected party                                     |
| 8    | `NotInOpenState`         | Expected status Open                                                 |
| 9    | `NotInFundedState`       | Expected status Funded                                               |
| 10   | `NotInDeliveredState`    | Expected status Delivered                                            |
| 11   | `NotInDisputedState`     | Expected status Disputed                                             |
| 12   | `ConfirmationWindowOpen` | Operation requires the confirmation window to have closed            |
| 13   | `ConfirmationWindowClosed` | Operation requires the confirmation window to still be open        |
| 14   | `EvidenceWindowOpen`     | Admin tried to resolve before evidence window elapsed                |
| 15   | `AllocationExceedsPot`   | Allocation components do not sum to `delivery_fee + rider_bond`      |
| 16   | `TimestampOverflow`      | Arithmetic overflow in timestamp or amount calculation               |
| 17   | `SelfDealNotAllowed`     | `customer == rider`                                                  |
| 18   | `AlreadyFunded`          | Reserved for future use                                              |

---

## Storage Layout & TTL

### Storage Keys

| Key                                | Storage Type | Value Type   | Purpose                       |
|------------------------------------|--------------|--------------|-------------------------------|
| `Admin`                            | Instance     | `Address`    | Contract administrator        |
| `Token`                            | Instance     | `Address`    | Token contract address        |
| `PlatformFeeWallet`                | Instance     | `Address`    | Where platform fees go        |
| `NextOrderId`                      | Instance     | `u64`        | Order ID counter              |
| `Order(u64)`                       | Persistent   | `Order`      | Order data                    |
| `CustomerOrders(Address)`          | Persistent   | `Vec<u64>`   | Order IDs by customer         |
| `RiderOrders(Address)`             | Persistent   | `Vec<u64>`   | Order IDs by rider            |

### Dynamic TTL for Persistent Storage

Persistent TTL extend is computed dynamically from the order's window parameters:

```
life_secs = confirmation_window_secs + evidence_window_secs
ttl_extend = max(life_secs * 2 / SECS_PER_LEDGER, PERSISTENT_TTL_EXTEND_MIN)
```

This ensures an order's data survives at least two full window cycles between mutating calls, even for orders with long evidence windows. Without this, a dispute could outlive its own storage TTL.

### TTL Bump Strategy

TTL bumps are performed only in mutating functions (`create_order`, `fund`, `mark_delivered`, `confirm_delivery`, `claim_after_timeout`, `dispute`, `resolve_dispute`, `mutual_cancel`). Query functions do not bump TTLs because they are typically executed via `simulateTransaction`, where state changes are discarded.

Fully resolved orders (`Released`, `Resolved`, `Cancelled`) eventually expire from persistent storage — this is by design. Off-chain indexers should ingest events for permanent records.

---

## Security Notes

### Why customer-confirms-primary

The customer is the only party with verifiable knowledge of correct delivery. A rider-confirms flow has been repeatedly exploited in two-sided delivery markets: the rider marks delivered, vanishes with the bond intact, and the customer has no recourse during the confirmation phase. The customer-confirms-primary model with a rider timeout escape valve is the converged best practice (Uber Eats, DoorDash, Wolt, Glovo all use variants of this).

### Why fund is atomic two-sided

If `fund_customer` and `stake_bond` were separate calls, a malicious rider could accept an order, never stake, and grief the customer by leaving their funds locked. Atomic two-sided funding eliminates this attack surface — the order either advances cleanly to `Funded` or fails entirely.

### Why explicit allocations in resolve_dispute

Rather than building a fixed set of "ruling templates," the admin supplies the full allocation and the contract validates that components sum to the pot. This:
- Forces the admin/arbiter to do their accounting before submitting
- Surfaces miscalculations on chain (better than silent rounding)
- Allows arbitrary splits without needing contract upgrades
- Makes the resolved event a complete audit record

### Limitations to be aware of

- **The admin is a trusted role.** A multi-arbiter pool with on-chain voting is left for V3; this contract optimizes for time-to-launch over admin decentralization.
- **No on-chain reputation.** Bonds are flat; risk-tiered bonds are an upstream concern handled by the off-chain matching layer.
- **No partial settlement on `claim_after_timeout`.** If the customer wanted to dispute but missed the window, the rider gets full standard payout. The off-chain UX must make the dispute window vivid.
- **`metadata_uri` is unverified.** The contract treats it as opaque. Frontend and off-chain indexer must validate.

---

## Pi Network specifics

- Built against `soroban-sdk = "22.0.0"`. Compatible with Pi Mainnet Protocol 23 (Soroban-enabled) once deployment access opens to third-party apps.
- Designed to be called from a Pi Browser-hosted frontend via the Pi SDK once Pi exposes Soroban invocation. In the interim, server-side Horizon RPC calls (with Pi Wallet signing for user transactions) provide an integration path.
- Native Pi is expected to be wrapped as a Stellar Asset Contract for use as the `token` parameter at construction.
