
## Research & Protocol Evolution

Research areas and papers that may inform future Lattice development.

### Trust Foundation (Future)

**Goal:** Ensure signed Wasm blobs can be independently verified as the output of known source code.

**Reproducible Build Attestation:**
- `BuildManifest` schema: `source_hash`, `toolchain`, `output_hash`, `builder_id`
- Store attestations alongside Wasm reference in sigchain
- Require N matching attestations before execution (configurable policy)

**Remote Attestation Protocol:**
- Attestation challenge/response in sync handshake
- Hardware roots of trust (TPM 2.0, Apple Secure Enclave, ARM TrustZone)
- Fallback: software attestation via signed kernel hash

| Challenge | Mitigation |
|-----------|------------|
| Rust/LLVM non-determinism | Pinned toolchain + `SOURCE_DATE_EPOCH` + locked `Cargo.lock` |
| Build environment variance | Nix flake or Docker for hermetic builds |
| Builder trust model | Named orgs, stake-weighted, or web-of-trust attesters |

### Byzantine Fork Detection
Formal framework for detecting equivocation (same seq# with different content). Generate `FraudProof` for permanent blocklisting.
- **Apply to:** `sync_state.rs` fork detection → punitive action
- **Ref:** [Kleppmann & Howard (2020)](https://arxiv.org/abs/2012.00472)

### Sybil Resistance & Gossip Scaling
Mechanisms to handle millions of nodes and prevent gossip flooding from malicious actors.
- **Problem:** Unbounded gossip from "millions of nodes" (Sybil attack) overwhelms bandwidth/storage.
- **Mitigation:** Resource constraints (PoW), Web-of-Trust gossip limits (only gossip for friends-of-friends), or reputation scores.

### Byzantine Fault Tolerance (BFT)
Ensure system resilience against malicious peers who may lie, omit messages, or attempt to corrupt state (beyond simple forks).
- **Objective:** Validated consistency without a central authority or global consensus.
- **Strategy:** Local verification of all data (SigChains), cryptographic prohibition of history rewriting, and detection/rejection of invalid CRDT merges.

