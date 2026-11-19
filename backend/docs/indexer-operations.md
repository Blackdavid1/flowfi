# Indexer Operations Guide

This document describes the externally observable behavior of the indexer's
lease coordination, the database indexes backing stream/event reads, the
admin replay preview/execute contract, the Soroban RPC retry/backoff policy,
graceful shutdown and poll cancellation, the indexer metrics, and the
blue-green database connection cutover procedure. It is written for operators
and on-call engineers. All public interfaces described here are additive;
existing endpoints and configuration keys keep working unless noted.

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
  the lease is rejected by the same fencing check that guards cursor writes.

---

## 4. Blue-green database connection cutover

### Objective

Move application instances (API and indexer worker) between database endpoints
during maintenance **without routing writes to an unverified target**. The
cutover is blue-green: the current endpoint (blue) keeps serving until the
target endpoint (green) passes readiness checks, and a single rollback path
returns traffic to blue if any check fails.

### Readiness checks (must all pass before routing writes)

Run these against the **target (green)** endpoint before switching:

1. **Reachability.** `pg_isready -h <green-host> -p <green-port>` succeeds.
2. **Schema parity.** The target reports the same applied migration version as
   the current endpoint (`SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1`).
3. **Replication lag.** If green is a replica being promoted, lag is below the
   configured threshold (`pg_last_wal_receive_lsn()` vs. `pg_last_wal_replay_lsn()`).
4. **Write probe.** A transaction-scoped probe write to a scratch table
   succeeds and is rolled back; no application data is mutated.
5. **Lease sanity.** The indexer lease table is present and readable on green
   so the worker can re-acquire ownership after cutover.

If any check fails, **do not cut over**; keep blue serving and investigate.

### Cutover procedure (operator commands)

1. **Pause the worker** so no cursor writes race the switch. The worker stops
   advancing the cursor and releases ownership locally; in-flight polls finish
   or are cancelled per the graceful-shutdown policy.
   ```
   kubectl -n <ns> scale deploy/indexer-worker --replicas=0
   ```
2. **Drain the API.** Remove API pods from the load balancer and wait for
   in-flight requests to complete (connection draining), so no request is
   mid-transaction during the switch.
   ```
   kubectl -n <ns> rollout pause deploy/api
   ```
3. **Run readiness checks** against green (section above). Abort on any
   failure.
4. **Point the connection secret at green** and restart consumers so they
   re-establish connections to the new endpoint.
   ```
   kubectl -n <ns> create secret generic db-conn \
     --from-literal=DATABASE_URL="$GREEN_DATABASE_URL" \
     --dry-run=client -o yaml | kubectl apply -f -
   kubectl -n <ns> rollout restart deploy/api
   ```
5. **Resume the worker** and confirm it re-acquires the lease and advances the
   cursor on green.
   ```
   kubectl -n <ns> scale deploy/indexer-worker --replicas=<n>
   ```
6. **Verify** API health and that the indexer cursor is advancing (metrics in
   section 6).

### Rollback procedure

Rollback is the same sequence with the endpoints swapped. Trigger rollback if
any readiness check fails after the switch, if the worker cannot re-acquire the
lease, or if API error rate rises above threshold.

1. **Pause the worker** (`scale deploy/indexer-worker --replicas=0`).
2. **Drain the API** (`rollout pause deploy/api`).
3. **Point the connection secret back at blue** and restart the API.
   ```
   kubectl -n <ns> create secret generic db-conn \
     --from-literal=DATABASE_URL="$BLUE_DATABASE_URL" \
     --dry-run=client -o yaml | kubectl apply -f -
   kubectl -n <ns> rollout restart deploy/api
   ```
4. **Resume the worker** and confirm the lease is re-acquired and the cursor
   advances on blue.

### Behavior during cutover

- **In-flight API requests** complete against the endpoint they started on;
  draining ensures no request spans the switch. New requests after restart use
  the new endpoint.
- **Worker pause/resume.** While paused, no cursor writes occur. On resume the
  worker re-establishes its connection pool and re-acquires the lease; fencing
  prevents a stale owner from writing to the wrong endpoint.
- **Connection re-establishment.** Restarting consumers forces fresh
  connections; pooled connections to the old endpoint are discarded rather than
  reused.

### Failure and boundary cases

- **Target unreachable.** Readiness check 1 fails; cutover aborts, blue keeps
  serving.
- **Partial cutover.** If the secret is updated but a consumer fails to
  restart, that consumer keeps its old connections until restarted; re-run the
  restart step. The worker stays paused until verified.
- **Rollback path.** Any post-switch failure follows the rollback procedure
  above; the worker is paused first so no writes land on the wrong endpoint
  mid-rollback.

---

## 5. Soroban RPC retry/backoff, graceful shutdown, and poll cancellation

See the sections below for the RPC retry/backoff policy, graceful shutdown, and
poll cancellation behavior. These are unchanged by the cutover procedure; the
worker pause/resume above reuses the existing graceful-shutdown path.

---

## 6. Indexer metrics

Operators watch the indexer metrics (lease acquisition/renewal, rejected stale
writes, cursor position, poll latency) to confirm health after a cutover. A
healthy post-cutover state shows the lease held by exactly one worker and the
cursor advancing.
