# Chain / Wallet Security Audit Plan (2026-05-28)

## Scope

This audit covers the chain and wallet security boundary across:

- `Rabbit-Chain-node`
- `rabbitchain-wallet-chrome`
- `rabbitchain-wallet-mobile`

Out of scope for this pass:

- `rabbitchain-site` marketing website
- `rabbitchain-explorer` UI/API internals, except where wallet or chain trust assumptions depend on it
- `rabbitchain-mining-stack`, except where chain RPC write-auth behavior affects mining submissions

## Audit Method

Each item is checked with:

- source review of the relevant code path
- existing test review
- targeted static searches for dangerous patterns
- targeted test/build execution where useful

Severity:

- `Critical`: direct key theft, forged chain state, arbitrary transaction signing, or unauthenticated state mutation with realistic exploitation path
- `High`: replay, network confusion, signer bypass, secret disclosure, persistent compromise, or unsafe default affecting mainnet
- `Medium`: hardening gap, missing validation, weak UX guardrail, or narrow abuse path
- `Low`: documentation, observability, or defense-in-depth gap

Status:

- `Planned`
- `Checking`
- `Pass`
- `Finding`
- `Blocked`

## Checklist

| ID | Area | Check | Repos | Status | Result |
| --- | --- | --- | --- | --- | --- |
| C1 | Chain RPC auth | State-changing RPC methods require auth and do not trust spoofed client IP headers for rate limits | `Rabbit-Chain-node` | Pass | Token enforcement and real remote-address limiter verified |
| C2 | Chain import path | Imported blocks validate hash, parent, difficulty, and PoW semantics before storage | `Rabbit-Chain-node` | Pass | Import path reuses header validation and rejects malformed blocks |
| C3 | Compute replay/network binding | Compute transactions must carry and match chain/network IDs before execution | `Rabbit-Chain-node`, wallets | Pass / Fixed | Simulate, submit, Chrome signing, and mobile signing now bind IDs |
| C4 | Compute signature model | Wallet-produced compute signing preimage matches node verification and rejects tampering | all | Pass | Shared signing domain and fixture parity verified |
| C5 | Chain storage/secret hygiene | Node and CLI do not log private keys, seed material, RPC tokens, or wallet secrets | `Rabbit-Chain-node` | Pass / Fixed | CLI secret files are `0600`; targeted log/secret scan reviewed |
| W1 | Chrome wallet key storage | Private keys are encrypted at rest and not exposed through extension messages/UI logs | `rabbitchain-wallet-chrome` | Pass | Vault encryption and public account sanitization verified |
| W2 | Chrome wallet RPC/network validation | Custom RPCs and selected networks cannot silently submit to the wrong chain | `rabbitchain-wallet-chrome` | Pass / Fixed | Write/preflight paths no longer auto-switch and bind selected network IDs |
| W3 | Chrome wallet message boundary | Extension message handlers do not allow arbitrary signing/export from untrusted contexts | `rabbitchain-wallet-chrome` | Pass / Fixed | Page bridge removed; unused page-injection permissions removed |
| W4 | Mobile wallet key storage | Private keys are encrypted or platform-keystore protected at rest and not logged | `rabbitchain-wallet-mobile` | Pass | Secure storage plus encrypted private-key payload reviewed |
| W5 | Mobile wallet RPC/network validation | Mobile RPC selection and compute submission enforce expected chain/network identity | `rabbitchain-wallet-mobile` | Pass / Fixed | HTTPS/local HTTP guard plus compute network binding reviewed |
| W6 | Mobile wallet signing boundary | UI/provider code cannot sign malformed or wrong-network compute transactions | `rabbitchain-wallet-mobile` | Pass / Fixed | Provider rejects invalid JSON shape and wrong-network IDs before signing |
| X1 | Cross-project parity | Wallet fixtures, canonical tx IDs, schemas, and validation rules match chain behavior | all | Pass | Rust and Chrome fixture tests pass; mobile signing domain reviewed |
| X2 | Dependency/secret scans | Dependency audit and static searches do not reveal known high-risk packages or committed secrets | all | Finding | No committed secrets found; Rust and Chrome dependency audit findings remain open |
| M1 | Mainnet runtime parity | Real `mainnet.sh` topology preserves mainnet chain IDs, RocksDb, default difficulty/limits, and RPC auth | `Rabbit-Chain-node` | Pass | Bootnode/follower/observer smoke passed and nodes were stopped |

## Findings

### F-01 High - Fixed - Compute network binding was incomplete on preflight/signing paths

`rabbit_submitComputeTx` already rejected chain/network mismatches, but `rabbit_simulateComputeTx`
and wallet signing paths could still accept or fill transaction data without first binding it to
the selected network.

Fixed:

- `rabbit_simulateComputeTx` now calls the same `validate_compute_tx_network` path as submit.
- Chrome wallet binds `chain_id` and `network_id` from the selected network before signing and
  rejects mismatched explicit IDs.
- Mobile wallet does the same before local signing.

Evidence:

- `Rabbit-Chain-node/crates/rabbitapi/src/rpc/mod.rs`: `rabbit_simulate_compute_tx`,
  `rabbit_submit_compute_tx`, `validate_compute_tx_network`
- `rabbitchain-wallet-chrome/src/background/index.ts`: `bindComputeTxToNetwork`
- `rabbitchain-wallet-mobile/lib/presentation/providers/wallet_provider.dart`:
  `bindComputeTxToNetwork`
- Tests: `test_rabbit_simulate_compute_tx_rejects_network_mismatch`,
  `test_rabbit_submit_compute_tx_rejects_network_mismatch`,
  Chrome `does not auto-switch networks for simulateComputeTx`,
  mobile `rejects wrong-network compute transactions before signing`

### F-02 Medium - Fixed - CLI wallet/session files were not forced to owner-only mode

Wallet and unlock-session files contain encrypted key material or session material. They are now
written through a single `write_secret_file` helper that creates new Unix files with mode `0600`,
tightens existing file permissions before writing, and syncs the file.

Evidence:

- `Rabbit-Chain-node/crates/rabbitcli/src/commands/wallet.rs`: `write_secret_file`
- Test: `saved_secret_files_are_owner_only`

### F-03 Low - Fixed - Chrome extension declared unused page-injection permissions

`activeTab` and `scripting` were declared but no `chrome.scripting` or page injection path is used.
The permissions were removed to reduce blast radius if the extension is compromised.

Evidence:

- `rabbitchain-wallet-chrome/manifest.json`
- Static search: no `chrome.scripting`, `content_scripts`, `onMessageExternal`, or
  `externally_connectable` entries

### F-04 High - Open - Rust dependency audit reports vulnerable transitive crates

`cargo audit` reports two vulnerability findings:

- `hickory-proto 0.24.4` via `libp2p-mdns 0.45.1`:
  `RUSTSEC-2026-0119`, CPU exhaustion during message encoding, fixed in `>=0.26.1`.
- `protobuf 2.28.0` via `prometheus 0.13.4`:
  `RUSTSEC-2024-0437`, uncontrolled recursion crash, fixed in `>=3.7.2`.

There are also unmaintained/unsound warnings for `bincode`, `core2`, `fxhash`, `instant`, `paste`,
`rustls-pemfile`, `lru`, and `rand`.

Recommended follow-up:

- Upgrade or replace the affected libp2p/hickory and prometheus/protobuf dependency paths in a
  dedicated compatibility branch.
- Re-run full chain, P2P, mining, RocksDb, and mainnet topology smoke tests after dependency
  changes.

### F-05 High - Open - Chrome dependency audit reports build/test supply-chain issues

`bun audit` reports 20 vulnerabilities: 8 high and 12 moderate. High findings include `undici`,
`rollup`, `flatted`, and `picomatch`; moderate findings include `postcss`, `vite`, `ws`, `yaml`,
`esbuild`, `brace-expansion`, and additional `picomatch` advisories.

Most affected paths are build/test/dev tooling, but they still matter for release integrity and CI
runner safety.

Recommended follow-up:

- Refresh `bun.lock` with compatible upgrades first.
- If compatible upgrades cannot clear high findings, plan the Vite/CRX plugin/test-tooling major
  upgrade separately.
- Re-run extension unit tests and production build after lockfile changes.

### F-06 Medium - Open - Strict mainnet RPC write auth needs wallet delivery design

The node now enforces token-protected stateful write methods. Chrome and mobile wallets do not
currently have an RPC auth-token configuration path, so direct submission to a strict mainnet node
will fail unless an authenticated gateway/proxy is used.

Security note:

- Do not simply embed a shared mainnet token in wallet builds.
- Prefer per-user/session gateway authorization, or a wallet setting that stores endpoint-specific
  tokens in encrypted/platform storage with clear UX warnings.

### F-07 Blocked - Mobile formatter/tests could not run in this environment

`dart` and `flutter` are not installed in this workspace environment, so mobile `dart format` and
Flutter tests could not be executed here. Source review and static searches were completed, and
new tests were added, but runtime verification is still pending on a Flutter-enabled runner.

## Checklist Results

| ID | Status | Result |
| --- | --- | --- |
| C1 | Pass | Stateful write RPC methods require token configuration; rate limiting uses `remote_addr.ip()` and does not trust `x-forwarded-for`. |
| C2 | Pass | `rabbit_importBlock` rejects legacy transactions, stale/parent mismatch, invalid header hash, invalid parent/header relation, invalid difficulty, invalid mix hash, and insufficient PoW. |
| C3 | Pass / Fixed | Submit and simulate both require tx chain/network IDs to match node config; wallets bind IDs before signing. |
| C4 | Pass | Chain, Chrome, and mobile compute signing use `RABBITCHAIN-COMPUTE-SIGNING-V1`; shared fixtures keep canonical tx ID parity. |
| C5 | Pass / Fixed | CLI wallet/session writes are owner-only on Unix; no direct private-key logging found in targeted scans. |
| W1 | Pass | Chrome vault stores private keys encrypted in `chrome.storage.local` using PBKDF2-SHA256 and AES-GCM; public account responses omit `privateKey`. |
| W2 | Pass / Fixed | Chrome compute simulate/submit no longer auto-switches RPC networks and binds tx IDs to selected network before signing. |
| W3 | Pass / Fixed | Page bridge is removed, no external message bridge was found, and unused page-injection permissions were removed. |
| W4 | Pass | Mobile stores wallet accounts in `FlutterSecureStorage`; private keys are encrypted with PBKDF2-SHA256 and AES-GCM before storage. |
| W5 | Pass / Fixed | Mobile custom RPC accepts HTTPS or local HTTP only; compute signing now binds selected chain/network IDs. |
| W6 | Pass / Fixed | Mobile provider rejects non-object/mismatched compute tx inputs before signing. |
| X1 | Pass | Cross-project fixture parity verified by Rust and Chrome tests; mobile uses the same signing domain string. |
| X2 | Finding | No committed secrets found in targeted scans, but Rust and Chrome dependency audits have open high-risk findings. |
| M1 | Pass | `mainnet.sh` bootnode/follower/observer topology started successfully with mainnet IDs, RocksDb files, default rate limit/difficulty, and RPC auth enabled. |

## Evidence Log

- Audit started on 2026-05-28.
- `cargo fmt --all` passed.
- `cargo fmt --all -- --check` passed after the final Rust edits.
- `cargo test -p rabbitapi -p rabbitcli` passed:
  `rabbitapi` 64 tests, `compute_redb_smoke` 1 test, `compute_smoke` 1 test,
  `rabbitcli` 6 tests.
- `npm test` in `rabbitchain-wallet-chrome` passed: 5 test files, 21 tests.
- `npm run build` in `rabbitchain-wallet-chrome` passed and produced a production extension build.
- `npx prettier --check manifest.json src/background/index.ts src/background/services/RpcService.ts tests/unit/services/RpcService.test.ts` passed.
- `cargo audit` failed with 2 vulnerability findings and 10 warnings; see F-04.
- `bun audit` failed with 20 vulnerability findings; see F-05.
- `npm audit --audit-level=high` is not applicable to `rabbitchain-wallet-chrome` because the project uses
  `bun.lock` and has no `package-lock.json`.
- `dart --version` and `flutter --version` failed with `command not found`; see F-07.
- Targeted static searches found no `dangerouslySetInnerHTML`, `innerHTML =`, `eval`,
  `new Function`, `chrome.runtime.onMessageExternal`, `externally_connectable`, `content_scripts`,
  or `chrome.scripting` usage in Chrome wallet source/manifest.
- Targeted static searches found no Rust `unsafe` blocks or shell command execution in chain
  crates. Expected wallet/session token handling was reviewed in CLI wallet code.
- Mainnet topology smoke:
  bootnode/follower/observer started via `scripts/mainnet.sh` with `--rpc-auth-token`.
  All returned `net_version` = `10086`.
  Bootnode log showed `Network profile: mainnet`, `chain_id: 10086`, `network_id: 10086`,
  `rpc auth: enabled`, `rpc rate limit: 600 req/min`,
  `Genesis difficulty: 1000000000000000`.
  Bootnode RocksDb directory contained `CURRENT`, `MANIFEST`, `OPTIONS`, `LOG`, `LOCK`, and
  `IDENTITY` files.
  Unauthenticated `rabbit_submitWork` and `rabbit_importBlock` returned `Unauthorized`.
  All three mainnet smoke nodes were stopped after verification.
