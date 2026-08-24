# gyema-contracts

Soroban smart contracts for [Gyema](https://github.com/tsotsoobi/gyema-app) — a decentralized peer-to-peer logistics app on Pi Network.

> **Status: deployed on Pi Testnet, unaudited, not on Mainnet.** Escrow v2 is deployed and proven on Pi Testnet only. It is not on Pi Mainnet, it is not wired into any shipped product, and no real value has ever passed through it. Do not use in production. See [Status](#status) below.

## What this is

A Cargo workspace of Soroban smart contracts intended for Pi Network Mainnet, currently deployed and exercised on Pi Testnet at Protocol 26. The first contract, `delivery_escrow`, implements a two-sided escrow for delivery orders: customers fund delivery fees, riders stake performance bonds, and funds release on customer confirmation, rider timeout claim, or admin-arbitrated dispute resolution.

This repo is a companion to [gyema-app](https://github.com/tsotsoobi/gyema-app), the customer-facing Pi Browser application. The app ships today on `Pi.createPayment()`, ahead of the December 19, 2026 gyema.pi domain claim deadline. These contracts are the escrow layer for a later product phase, and their Mainnet deployment waits on Pi Core Team guidance.

A note on naming, because two numbering schemes meet in this repo. Product phases are described in words here, not numbers. Where you see escrow v1 and escrow v2, those are deployed contract generations: escrow v1 is abandoned, escrow v2 is the current Pi Testnet instance and the code in this repo.

## Contracts

| Contract | Purpose | Status |
|---|---|---|
| [`delivery_escrow`](contracts/delivery_escrow/) | Two-sided delivery escrow with dispute resolution | Escrow v2 deployed on Pi Testnet; not on Mainnet |

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

The contracts in this repo follow the patterns established by [PiNetwork/SmartContracts](https://github.com/PiNetwork/SmartContracts) (the Pi Core Team's reference subscription contract): same workspace structure, the same TTL management discipline, the same error / event / storage-key conventions. This repo pins `soroban-sdk = "23.5.3"`, per the workspace `Cargo.toml`. This deliberate alignment is intended to make the contracts ergonomic for anyone familiar with the official reference and easier to review by the Pi developer community.

For the delivery escrow specifically, the design optimizes for known fraud-resistance patterns in two-sided marketplaces:

- **Customer-confirms primary.** Only the customer can verify correct delivery. Rider self-confirmation is a known fraud vector and is not offered.
- **Rider timeout escape valve.** Customer silence past the confirmation window allows the rider to claim, preventing customer-side griefing.
- **Split two-sided funding.** Customer's fee and rider's bond arrive in separate single-signature transactions; unilateral withdrawal while un-funded eliminates asymmetric grief cases.
- **Explicit allocation in disputes.** Admin must supply payouts that sum to exactly the pot — surfaces accounting mistakes on chain.
- **Three-pot escrow.** Delivery fee, rider bond, and platform fee tracked separately for clean accounting and waiver flexibility.

Full design rationale and tradeoffs are in [`contracts/delivery_escrow/README.md`](contracts/delivery_escrow/README.md).

## Status

**Deployed on Pi Testnet. Not on Mainnet. At rest.** Where things stand:

1. Escrow v2 is deployed on Pi Testnet and its lifecycle has been exercised there. Nothing is deployed to Pi Mainnet, nothing is reachable by Gyema app users, and no real value has ever passed through it.
2. Gyema's product ships on `Pi.createPayment()` ahead of the December 19, 2026 gyema.pi domain claim deadline.
3. These contracts have not been independently audited.
4. Mainnet deployment waits on Pi Core Team guidance.

**What you can safely do today:** read the code, run the tests locally, suggest improvements via Issues or PRs, fork for your own experiments.

**What you should not do:** route real value through these contracts, on any network. Testnet deployment is not a readiness signal. Wait for an audit and for explicit production-readiness in this README.

## Roadmap

- **Now:** escrow v2 deployed and proven on Pi Testnet. Track at rest.
- **When Mainnet deployment is cleared:** security audit, integration with the `gyema-app` frontend, Mainnet deployment.
- **Post-volume:** multi-arbiter dispute resolution (currently single-admin), risk-tiered bonds, cross-app composability.

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
