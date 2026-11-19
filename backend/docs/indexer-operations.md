# Indexer Operations Guide

This document describes the externally observable behavior of the indexer's
lease coordination, the database indexes backing stream/event reads, the
admin replay preview/execute contract, the Soroban RPC retry/backoff policy,
graceful shutdown and poll cancellation, and the indexer metrics. It is
written for operators and on-call engineers. All public interfaces described
here are additive; existing endpoints and configuration keys keep working
unless noted.

---

## 0. Multi-network deployment decision guide

This section explains how operators select and verify the **network, contract,
token, endpoint, and release artifact** combination for each environment. It is
the first thing to read before deploying or promoting a release. The values
below are verified against the current deployment configuration
(`.github/workflows/deploy-contracts.yml`, the testnet e2e config, and the
backend env example); if those files change, update this guide in the same PR.

### 0.1 Environments and their fixed combinations

Each environment pins a single, consistent combination. Never mix values from
different rows.

| Environment | Network / passphrase | Contract source | Token | RPC / Horizon endpoint | Release artifact |
| --- | --- | --- | --- | --- | --- |
| `local` | `Standalone` (`Standalone Network ; February 2017`) | built from working tree | locally issued test token | `http://localhost:8000/soroban/rpc`, `http://localhost:8000` | none (source build) |
| `testnet` | `Test SDF Network ; September 2015` | deployed by `deploy-contracts.yml` | testnet token from deploy output | `https://soroban-testnet.stellar.org`, `https://horizon-testnet.stellar.org` | tagged release artifact |
| `mainnet` | `Public Global Stellar Network ; September 2015` | deployed by `deploy-contracts.yml` (protected env) | mainnet token from deploy output | `https://soroban-mainnet.stellar.org`, `https://horizon.stellar.org` | tagged release artifact |

Rules that follow from the table:

- **Network passphrase is authoritative.** The backend must be configured with
  the passphrase matching the RPC endpoint. A mismatch causes signature
  verification to fail; do not "fix" it by disabling validation.
- **Contract and token IDs are per-network.** A contract ID from testnet is
  meaningless on mainnet. Always take contract and token IDs from the deploy
  output of the *same* environment.
- **Endpoints are per-network.** RPC and Horizon must both point at the same
  network. Mixing a testnet RPC with a mainnet Horizon URL is a misconfiguration.
- **Release artifacts are immutable.** Promote the exact artifact that was
  built and tested for the target network; do not rebuild during promotion.

### 0.2 Preflight checklist

Run through this checklist before every deploy or promotion. Each item maps to
an automated check where one exists.

1. **Network passphrase** matches the target environment (see table).
2. **RPC endpoint** host matches the network (testnet vs mainnet host).
3. **Horizon endpoint** host matches the same network as the RPC endpoint.
4. **Contract ID** was produced by the deploy run for this environment and is
   present in the deploy output / env file.
5. **Token contract ID** comes from the same deploy output as the contract ID.
6. **Release artifact** is the tagged artifact built for this network; its
   checksum matches the published release.
7. **Secrets** (deployer key, admin key) are provided by the environment's
   secret store — never committed and never copied between environments.
8. **Backend env** has no leftover values from another environment (grep the
   env file for the other network's hostnames and passphrases).

### 0.3 Safe failure examples

These are the expected, safe outcomes when a combination is wrong. In every
case the system must **fail closed** — refuse to start or refuse the operation
— rather than silently proceeding.

- **Passphrase / endpoint mismatch.** Backend configured with the testnet
  passphrase but a mainnet RPC URL: startup validation rejects the config and
  the process exits non-zero. It does not fall back to a default network.
- **Cross-network contract ID.** A testnet contract ID supplied on mainnet:
  contract calls fail with a not-found / invalid-contract error; the indexer
  does not advance the cursor past the failed ledger.
- **Token from the wrong network.** A token ID that does not exist on the
  configured network: token reads fail explicitly; balances are not reported
  as zero.
- **Stale release artifact.** An artifact whose checksum does not match the
  published release for the target network: promotion is blocked before
  deploy.
- **Leaked cross-environment secret.** A deployer key from another environment:
  the deploy transaction is rejected by the network (wrong account / wrong
  network); the deploy job fails and does not retry with a different key.

If any of these occur, stop, correct the configuration, and re-run the
preflight checklist. Do not weaken network or chain validation to make a
deploy pass.

---

## 1. Poll ownership lease

### Behavior

The indexer uses a single renewable lease stored in the database to guarantee
that **at most one backend replica actively polls and advances the cursor** at
any time.

- **Single owner.** A worker acquires the lease before polling. Only the
  current lease owner may advance the indexer cursor. Non-owners idle and
  periodically attempt acquisition.
- **Owner identity.** Each worker has a unique owner ID (hostname + process
  identity + random suffix). The lease records the current owner ID and a
  monotonically increasing **fencing token** (generation number).
- **TTL and renewal.** The lease has a finite TTL. The owner renews it on a
  heartbeat well before expiry. If renewal fails (e.g., DB unavailable), the
  worker stops advancing the cursor and releases ownership locally.
- **Takeover.** If the lease expires without renewal (owner crash, network
  partition, or shutdown), another worker may acquire it. Acquisition
  increments the fencing token.
- **Fencing.** Every cursor write is conditional on the writer still holding
  the lease at the current fencing token. A stale owner whose lease has been
  taken over will have its writes **rejected** rather than overwriting the new
  owner's progress. Rejected writes are logged and counted (see metrics).

### Externally observable effects

- At most one replica advances the cursor at a time; the cursor is monotonic
  per fencing generation.
- After an owner is lost, another replica takes over within roughly one TTL
  plus the acquisition retry interval.
- Stale writes from a former owner are discarded; they never move the cursor
  backwards or skip ledgers.

### Configuration

| Key | Meaning | Notes |
| --- | --- | --- |
| `INDEXER_LEASE_TTL_MS` | Lease time-to-live | Defaults are safe; must exceed renewal interval |
| `INDEXER_LEASE_RENEW_INTERVAL_MS` | Renewal heartbeat interval | Must be well below TTL |
| `INDEXER_LEASE_ACQUIRE_RETRY_MS` | Retry interval for non-owners | Controls takeover latency |

### Rollout and migration

- The lease table is created by migration and is additive; no existing data is
  rewritten.
- **Rolling upgrade:** during a mixed-version rollout, older replicas that do
  not participate in the lease must not run concurrently with new replicas
  against the same cursor. Recommended sequence:
  1. Apply the migration.
  2. Scale the indexer to a single replica (or stop old replicas).
  3. Deploy the lease-aware version.
  4. Scale back up; the lease ensures a single active owner.
- **Rollback:** the lease table may remain in place; older versions ignore it.
  Ensure only one replica runs if rolling back to a non-lease version.

---

## 2. Database indexes for stream and event reads

### Added indexes

The following indexes were added after profiling representative PostgreSQL
query plans for list, history, analytics, and cursor queries. Each is
migration-safe (created with `CREATE INDEX`; no data rewrite) and preserves
result ordering and pagination semantics.

| Index | Table | Columns | Hot path served |
| --- | --- | --- | --- |
| `streams_created_at_id_idx` | `streams` | `(created_at DESC, id DESC)` | Stream list ordering + keyset pagination |
| `streams_sender_idx` | `streams` | `(sender, created_at DESC)` | Per-wallet stream history |
| `events_stream_id_ledger_idx` | `events` | `(stream_id, ledger DESC)` | Per-stream event history |
| `events_ledger_idx` | `events` | `(ledger)` | Cursor scan / ledger range reads |
| `events_type_ledger_idx` | `events` | `(type, ledger DESC)` | Analytics by event type |

### Query-plan evidence

Before/after `EXPLAIN (ANALYZE, BUFFERS)` was captured for each hot path on a
representative dataset. Summary:

- **Stream list (keyset pagination):** before — sequential scan + sort;
  after — index scan on `streams_created_at_id_idx`, no sort node, stable
  ordering by `(created_at, id)`.
- **Per-wallet history:** before — filter + sort over `streams`; after —
  index scan on `streams_sender_idx`.
- **Per-stream events:** before — filter on `stream_id` then sort; after —
  index scan on `events_stream_id_ledger_idx`.
- **Cursor scan / ledger range:** before — sequential scan; after — index
  scan on `events_ledger_idx`.
- **Analytics by type:** before — filter + sort; after — index scan on
  `events_type_ledger_idx`.

### Operational impact

- Index creation takes a brief lock; on large tables run during a low-traffic
  window or use the provided migration which creates indexes without blocking
  writes where supported.
- Additional write amplification on `events` and `streams` inserts is modest
  and was measured within acceptable bounds.
- No public query interfaces changed; ordering and pagination are preserved.

---

## 3. Admin replay: preview and execution

### Contract

Replay is exposed under the existing admin replay controls. Two operations:

- **Preview** (`preview`): computes the affected ledger range and the expected
  record changes **without performing any writes**. Safe to call repeatedly.
- **Execute** (`execute`): performs the replay and returns an execution report.
  Supports retry.

Both accept the same replay parameters as before (ledger range / target). The
response is a **report** with the following fields:

| Field | Description |
| --- | --- |
| `requestId` | Correlation ID for this preview/execute call |
| `mode` | `preview` or `execute` |
| `ledgerRange.from` / `ledgerRange.to` | Affected ledger range (inclusive) |
| `expectedChanges` | Preview only: counts of records that would be created/updated/deleted |
| `counts` | Execute only: records created/updated/deleted/skipped |
| `durationMs` | Wall-clock duration of the operation |
| `status` | `succeeded` / `failed` / `partial` |
| `failurePosition` | On failure: the ledger at which execution stopped (null on success) |
| `liveCursor` | The live indexer cursor after the operation |

### Guarantees

- **Preview performs no writes.** It is read-only and reproducible.
- **Execution reports are reproducible** for the same inputs and starting
  state; retries produce a fresh `requestId` and report.
- **Live cursor is preserved on failure.** If execution fails partway, the
  live cursor is not advanced past the last successfully applied ledger, and
  `failurePosition` identifies where it stopped. Retry resumes from there.

### Concurrency: replay vs. live polling

Replay, reset, and normal polling all mutate the same shared cursor and event
state, so they are **serialized** against each other. The serialization is
enforced at the database layer (a transaction-scoped advisory lock keyed on the
indexer cursor row), not in process memory, so it holds **across multiple
backend instances**.

- **Mutual exclusion.** While a replay (or reset) holds the lock, the live
  worker cannot advance the cursor; while the live worker holds it, replay
  waits. Operations queue rather than interleave.
- **Monotonic cursor.** The cursor never moves backwards as a result of a
  replay overlapping a poll. A replay that would move the cursor behind the
  live position is rejected (reported as `status: failed` with a clear
  message) instead of silently rewinding it.
- **Deterministic event state.** Because replay and polling never run
  concurrently, the resulting event state for a given starting state and
  inputs is deterministic regardless of which replica handled the poll or the
  replay.
- **Fencing still applies.** A replay executed by a replica that does not hold
  the lease is rejected by the same fencing check as cursor writes.
