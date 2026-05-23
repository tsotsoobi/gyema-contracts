# gyema-contracts

Soroban smart contracts for [Gyema](https://github.com/tsotsoobi/gyema-app) — a decentralized peer-to-peer logistics app on Pi Network.

> **Status: pre-deployment, unaudited.** These contracts are not yet deployed to any network. Do not use in production. See [Status](#status) below.

## What this is

A Cargo workspace of Soroban smart contracts intended for Pi Network Mainnet (Protocol 23+, Stellar Core v23.0.1). The first contract, `delivery_escrow`, implements a two-sided escrow for delivery orders: customers fund delivery fees, riders stake performance bonds, and funds release on customer confirmation, rider timeout claim, or admin-arbitrated dispute resolution.

This repo is a companion to [gyema-app](https://github.com/tsotsoobi/gyema-app), the customer-facing Pi Browser application. The app currently uses `Pi.createPayment()` for V1 (launching ahead of the December 19, 2026 gyema.pi domain claim deadline). These contracts are the V2 layer, intended for deployment once the Pi Core Team opens Soroban deployment access to third-party apps.

## Contracts

| Contract | Purpose | Status |
|---|---|---|
| [`delivery_escrow`](contracts/delivery_escrow/) | Two-sided delivery escrow with dispute resolution | Reference implementation; not deployed |

## Quick start

Prerequisites: Rust toolchain (`rustup`) and the Soroban target.

```bash
# Install Rust if needed: https://rustup.rs
rustup target add wasm32-unknown-unknown

# Optional: Stellar CLI for deployment work later
# https://developers.stellar.org/docs/tools/developer-tools/cli

git clone https://github.com/tsotsoobi/gyema-contracts.git
cd gyema-contracts

# Type-check the workspace
cargo check

# Run the test suite
cargo test

# Build release WASM (optimized for Soroban)
cargo build --release --target wasm32-unknown-unknown
```

## Repository layout

```
gyema-contracts/
├── Cargo.toml                              workspace manifest
├── LICENSE                                 Apache-2.0
├── README.md                               this file
├── .github/workflows/rust.yml              CI: cargo check + cargo test
└── contracts/
    └── delivery_escrow/
        ├── Cargo.toml                      crate manifest
        ├── README.md                       full API reference
        └── src/
            ├── lib.rs                      contract source
            └── test.rs                     test suite
```

## Design philosophy

The contracts in this repo follow the patterns established by [PiNetwork/SmartContracts](https://github.com/PiNetwork/SmartContracts) (the Pi Core Team's reference subscription contract): same workspace structure, same `soroban-sdk = "22.0.0"` pin, same TTL management discipline, same error / event / storage-key conventions. This deliberate alignment is intended to make the contracts ergonomic for anyone familiar with the official reference and easier to review by the Pi developer community.

For the delivery escrow specifically, the design optimizes for known fraud-resistance patterns in two-sided marketplaces:

- **Customer-confirms primary.** Only the customer can verify correct delivery. Rider self-confirmation is a known fraud vector and is not offered.
- **Rider timeout escape valve.** Customer silence past the confirmation window allows the rider to claim, preventing customer-side griefing.
- **Atomic two-sided funding.** Customer's fee and rider's bond move in a single transaction, eliminating asymmetric grief cases.
- **Explicit allocation in disputes.** Admin must supply payouts that sum to exactly the pot — surfaces accounting mistakes on chain.
- **Three-pot escrow.** Delivery fee, rider bond, and platform fee tracked separately for clean accounting and waiver flexibility.

Full design rationale and tradeoffs are in [`contracts/delivery_escrow/README.md`](contracts/delivery_escrow/README.md).

## Status

**Pre-deployment.** As of the latest commit, no contract in this repo has been deployed to Pi Mainnet or Testnet. The reasons:

1. The Pi Core Team announced Protocol 23 (Soroban enablement) Mainnet rollout on May 20, 2026 but has not yet published the developer pipeline for third-party apps to deploy Soroban contracts.
2. Gyema's V1 product launch (using `Pi.createPayment()` only) is the priority through December 19, 2026.
3. These contracts have not been independently audited.

**What you can safely do today:** read the code, run the tests locally, suggest improvements via Issues or PRs, fork for your own experiments.

**What you should not do:** deploy to a live network and route real funds through these contracts. Wait for an audit and explicit production-readiness in this README.

## Roadmap

- **V1 (current):** `delivery_escrow` reference implementation. Local tests pass.
- **V2 (post Pi Mainnet Soroban access):** Testnet deployment, integration with `gyema-app` frontend, security audit, mainnet deployment.
- **V3 (post-volume):** Multi-arbiter dispute resolution (currently single-admin), risk-tiered bonds, cross-app composability.

## Contributing

Issues and PRs welcome. For substantive design changes, open an Issue first to discuss. The contract's design rationale is in `contracts/delivery_escrow/README.md` — please skim it before proposing changes that affect the state machine, release model, or dispute path.

## Related

- [gyema-app](https://github.com/tsotsoobi/gyema-app) — the Pi Browser application
- [Pi Logistics Ltd.](https://pillgh.com) — the company behind Gyema
- [PiNetwork/SmartContracts](https://github.com/PiNetwork/SmartContracts) — Pi Core Team's reference Soroban contracts
- [PiNetwork/PiRC](https://github.com/PiNetwork/PiRC) — Pi Requests for Comment (ecosystem standards)
- [Soroban documentation](https://developers.stellar.org/docs/build/smart-contracts/overview) — Stellar's smart contract platform

## License

Apache License 2.0. See [LICENSE](LICENSE).

## Contact

Maintainer: [@pillghana](https://x.com/pillghana) — Pi Logistics Ltd., Accra, Ghana.
