# RabbitChain RFC: Standard Block Body Model Migration

> Status: Implemented
>
> Last updated: 2026-06-03
>
> Scope: block format, body persistence, receipts, transaction roots, sync, RPC, explorer indexing

> Note: this document records the rollout from the legacy compatibility path to the body-first
> canonical block model. Historical references to the old block layout are kept only where they
> clarify compatibility boundaries.

## 1. Summary

RabbitChain now uses a body-first canonical block model: the `Block` type carries `body` alongside
`header + uncles`, while transaction results are represented through canonical receipts and
compatibility indexes. This RFC records the migration from the legacy compatibility path to the
standard block-body model.

This RFC documents a staged migration from the legacy compatibility path to a standard block model
with:

- a canonical `BlockBody`
- transaction commitments via `transactions_root`
- execution receipts via `receipts_root`
- explicit block-body sync and RPC retrieval
- backward-compatible reading of legacy blocks during the transition

The migration is designed to be incremental:

1. Add body and receipt data structures alongside the legacy compatibility path.
2. Persist and serve block bodies without changing consensus.
3. Make transaction and receipt roots canonical and verifiable.
4. Activate the standard block format at a chosen height.
5. Retire legacy compatibility assumptions from the hot path.

## 2. Motivation

The legacy model was acceptable for an MVP execution chain, but it had clear limitations:

- The legacy block layout could not answer “how many transactions are in a block”; the
  body-first model fixes that by moving transactions into `BlockBody`.
- `transactions_root` and `receipts_root` now back canonical body commitments for newly produced
  blocks.
- Receipts, block bodies, and compute transaction results remain split across canonical and
  compatibility indexes.
- Sync and RPC semantics are easier to reason about with an explicit block/body/receipt model.
- Long-term compatibility with ecosystem tooling is stronger with a canonical block-body design.

The goal of this migration is not to make the system more complex immediately. The goal is to move
from a minimal execution envelope to a standard, auditable, and externally legible block format
without breaking the chain in one step.

## 3. Current State

### 3.1 Canonical block shape

Current `Block` shape:

```rust
pub struct Block {
    pub header: BlockHeader,
    pub body: Option<BlockBody>,
    pub uncles: Vec<BlockHeader>,
}
```

Current `BlockHeader` already contains:

- `state_root`
- `transactions_root`
- `receipts_root`
- `gas_limit`
- `gas_used`
- `coinbase`
- `difficulty`
- `nonce`
- `extra_data`
- `mix_hash`

For newly produced blocks, these fields are populated from the canonical block body. The `Option`
wrapper remains for historical reads and compatibility with older records.

### 3.2 Current execution and mining behavior

Current mining/production logic constructs a body-bearing block and stores it canonically. Legacy
records without a body remain readable, but they are no longer the canonical production shape.

### 3.3 Current sync behavior

The sync protocol uses:

- `SyncHeader` for block envelope propagation
- `SyncBlockBody` for body retrieval and validation
- `SyncStateSnapshot` for state/index recovery

`SyncBlockBody` carries the canonical body payload for body-bearing blocks, while legacy records
remain readable through compatibility paths.

### 3.4 Current compute transaction tracking

Compute transaction outcomes are still indexed independently for compatibility and query
convenience, but receipts are the canonical execution outcome surface.

## 4. Goals

### 4.1 Functional goals

- Introduce a canonical `BlockBody`.
- Store canonical transactions in block bodies.
- Compute `transactions_root` from the body.
- Compute `receipts_root` from execution receipts.
- Preserve block replay and block-by-block verification.
- Keep legacy blocks readable during transition.

### 4.2 Non-functional goals

- Keep the migration incremental and reversible until activation.
- Avoid breaking current compute benchmarks during the early phases.
- Keep sync and RPC backward-compatible while both formats coexist.
- Make the format explicit enough for explorers, indexers, and external tooling.

## 5. Non-goals

- Replacing PoW consensus in this RFC.
- Rewriting the entire compute execution model.
- Designing a full fee market in the first migration step.
- Removing all legacy compatibility paths on day one.
- Optimizing the body format for archive analytics before canonical correctness exists.

## 6. Proposed Target Model

### 6.1 Canonical data model

The target model should be:

```text
Block
  - header
  - body
  - uncles

BlockHeader
  - parent_hash
  - state_root
  - transactions_root
  - receipts_root
  - gas_limit
  - gas_used
  - number
  - timestamp
  - difficulty
  - nonce
  - coinbase
  - mix_hash
  - extra_data
  - version

BlockBody
  - transactions
  - receipts
  - optional metadata
```

### 6.2 Canonical flow

```text
tx admission
  -> ordering / block assembly
  -> execution
  -> receipts
  -> transactions_root + receipts_root
  -> header
  -> block body persistence
  -> sync / RPC / explorer
```

### 6.3 Root semantics

- `transactions_root` commits to the ordered list of transactions in the block body.
- `receipts_root` commits to the ordered list of execution receipts.
- `state_root` commits to post-execution state after applying the body.

The exact commitment scheme can be finalized during implementation, but it must be deterministic,
versioned, and canonical for a given activation height.

## 7. Data Structure Model

This section reflects the current canonical model and compatibility envelope.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub body: Option<BlockBody>,
    pub uncles: Vec<BlockHeader>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockBody {
    pub transactions: Vec<TxEnvelope>,
    pub receipts: Vec<Receipt>,
    pub metadata: BlockBodyMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockBodyMetadata {
    pub tx_count: u32,
    pub body_hash: Hash,
    pub codec_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxEnvelope {
    pub tx_id: Hash,
    pub domain_id: u64,
    pub chain_id: u64,
    pub network_id: u64,
    pub command: String,
    pub nonce: u64,
    pub input_set: Vec<Hash>,
    pub read_set: Vec<Hash>,
    pub output_proposals: Vec<OutputProposal>,
    pub payload: Vec<u8>,
    pub witness: TxWitness,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_id: Hash,
    pub block_hash: Hash,
    pub status: ReceiptStatus,
    pub gas_used: u64,
    pub compute_units: u64,
    pub logs: Vec<ReceiptLog>,
    pub output_refs: Vec<OutputId>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReceiptStatus {
    Success,
    Reverted,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceiptLog {
    pub topic: String,
    pub data: Vec<u8>,
}
```

### 7.1 Notes on the model

- `body: Option<BlockBody>` keeps backward compatibility for historical reads and sync replay.
- `TxEnvelope` is the canonical serialized transaction shape inside a block body.
- `Receipt` is the canonical execution outcome and should become the primary query surface.
- `BlockBodyMetadata` can carry versioning and body-level integrity checks.

## 8. Migration Phases

### Phase P0: Sidecar introduction

Goal:

- Introduce body and receipt types without changing consensus behavior.

Changes:

- Add `BlockBody`, `Receipt`, and related serializers.
- Persist block bodies alongside existing block headers.
- Expose block body retrieval through RPC.
- Keep consensus behavior unchanged while introducing body sidecars.

Acceptance:

- Existing nodes still start and sync.
- Existing benchmarks still pass.
- Body data can be written and read as a sidecar.
- Legacy blocks remain readable.

### Phase P1: Canonical body persistence

Goal:

- Make the block body part of the canonical storage layout.

Changes:

- Persist body data under a canonical key/index by block hash.
- Add RPC endpoints or flags to retrieve block + body + receipts together.
- Persist receipts with stable ordering.

Acceptance:

- For any new block, body retrieval by hash works.
- Receipts are stored and retrievable by tx hash and block hash.
- Block body and header are linked deterministically.

### Phase P2: Root computation and verification

Goal:

- Make `transactions_root` and `receipts_root` real commitments.

Changes:

- Compute `transactions_root` from the ordered body transactions.
- Compute `receipts_root` from the ordered receipts.
- Validate roots on import, sync, and replay.
- Add root mismatch rejection paths.

Acceptance:

- A block with a tampered body fails root verification.
- Header/body/receipt round-trip is deterministic.
- Block replay reproduces the same roots.

### Phase P3: Activation height and canonical switch

Goal:

- Turn the standard block format on at a specific activation height.

Changes:

- Introduce versioned block format rules.
- Mark the activation height in config or chain params.
- After activation, canonical blocks must include a valid body and receipts.

Acceptance:

- Blocks below activation height remain readable.
- Blocks at and above activation height must satisfy body/root rules.
- Fork choice and sync remain stable across the transition boundary.

### Phase P4: Remove legacy compatibility assumptions from hot paths

Goal:

- Make the standard block body the default mental model everywhere.

Changes:

- Update explorer/indexer assumptions.
- Remove code paths that assume legacy blocks are canonical.
- Rebase RPC docs and CLI output around block/body/receipt terminology.

Acceptance:

- Legacy blocks are no longer produced on the canonical path.
- All public docs describe block + body + receipt semantics.
- Legacy compatibility is limited to historical reads.

## 9. Compatibility Strategy

### 9.1 Versioning

Any new body or receipt encoding must carry an explicit version prefix.

### 9.2 Legacy read support

Readers should accept:

- legacy blocks
- new block format blocks

Writers may switch to the new format only after the read path is stable.

### 9.3 Activation gating

The canonical switch must be height-gated or version-gated so that:

- old blocks can still be imported and queried
- new blocks are not ambiguous
- reorg handling is deterministic across the cutover

## 10. Risks

- Root mismatch bugs during the transition.
- Body/receipt ordering mismatches between builder and verifier.
- Sync edge cases when one node understands the new body format and another still speaks the old one.
- RPC and explorer incompatibility if schema changes are not versioned.
- Benchmark regressions if body persistence is added without batching or codec care.

## 11. Testing and Verification Plan

### Unit tests

- Block body serialization round-trip.
- Transaction root calculation.
- Receipt root calculation.
- Root mismatch rejection.

### Integration tests

- Block production with body persistence.
- Importing a block with matching body and roots.
- Rejecting a block with tampered body or receipts.
- Reading legacy blocks.

### Sync tests

- Header-first sync for old blocks.
- Body retrieval for new blocks.
- Snapshot and body coexistence.

### RPC tests

- Query block by hash with and without body.
- Query receipts by tx hash.
- Query block body by hash.

### Benchmark tests

- Compare legacy compatibility-path throughput with body-first throughput.
- Measure the cost of root computation and body persistence separately.

## 12. Acceptance Checklist

### P0 acceptance

- [ ] `BlockBody` and `Receipt` types exist.
- [ ] Body data can be persisted and read back.
- [ ] No consensus behavior changes yet.
- [ ] Existing tests and benchmarks still pass.

### P1 acceptance

- [ ] New blocks can store a canonical body.
- [ ] Receipts are written and indexed.
- [ ] RPC can return body and receipts.
- [ ] Legacy blocks still load correctly.

### P2 acceptance

- [ ] `transactions_root` is computed from the body.
- [ ] `receipts_root` is computed from receipts.
- [ ] Import and replay reject mismatched roots.
- [ ] Deterministic replay passes on multiple nodes.

### P3 acceptance

- [ ] An activation height is configured.
- [ ] Pre-activation blocks still verify.
- [ ] Post-activation blocks require body/root correctness.
- [ ] Reorg across the boundary works.

### P4 acceptance

- [ ] Legacy compatibility assumptions are removed from canonical hot paths.
- [ ] Public docs describe the canonical block body model.
- [ ] Explorer and RPC use receipt/body terminology by default.
- [ ] Legacy read support remains for historical data only.

## 13. Open Questions

- Which commitment scheme should be used for `transactions_root` and `receipts_root`?
- Should `BlockBody` be embedded in the same storage envelope as `Block`, or kept as a sidecar by
  default with a canonical pointer?
- Should receipts be stored as a per-block ordered array, a per-tx index, or both?
- Should compute results remain a separate index for compatibility, or be folded entirely into
  receipts once the new model is canonical?
- What activation-height strategy best fits the current release cadence?

## 14. Implementation Record

The staged rollout described above has already been implemented in the codebase.

Keep this RFC as the design and compatibility reference for future changes, especially around:

- versioned roots
- legacy read support
- activation-height handling for future upgrades
