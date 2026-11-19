# Backend Architecture

This document is the canonical reference for backend service architecture. It is
referenced by `backend/src/services/indexerService.ts` and
`backend/src/services/soroban-indexer.service.ts` as the authoritative source for
indexer ownership, SSE broadcast flow, and keeper-key authorization.

For the full project-wide architecture (event type data flows, pause/resume
timing, environment variables, and operational runbook) see
[`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md).

---

## Indexer Ownership Model

Three files with overlapping names handle indexing and indexer management. Only
one of them is the source of truth for stream state.

| File | Role | Status |
|------|------|--------|
| `src/workers/soroban-event-worker.ts` (`SorobanEventWorker`) | **Source-of-truth indexer.** Polls Soroban RPC, decodes XDR events, persists `Stream` / `StreamEvent` rows, advances the `IndexerState` cursor, and broadcasts SSE updates. | **Active / source of truth.** Started by `src/workers/index.ts`. |
| `src/services/soroban-indexer.service.ts` (`SorobanIndexerService`) | **Legacy indexer being phased out.** A simpler duplicate poller that writes to the same DB rows and races with the worker on the same `Stream` / `StreamEvent` records (issue #801). | **Legacy — do not extend.** Removal tracked with functional consolidation (issue #801). Started directly from `src/index.ts`. |
| `src/services/indexerService.ts` | **Not an indexer at all.** Admin control-plane helpers (`getIndexerStatus`, `resetIndexer`, `replayFromLedger`) that read/reset the shared `IndexerState` cursor row and trigger the worker's poll loop. | **Active.** Name is misleading; kept alongside the legacy indexer above. |

### Key Rules

1. **When debugging indexing, read `src/workers/soroban-event-worker.ts`
   first.** It is the only file that persists canonical stream state.
2. **Do not add new behavior to `soroban-indexer.service.ts`.** It exists only
   for backwards compatibility while the double-indexer race (issue #801) is
   consolidated. Mirror any changes in `SorobanEventWorker` instead.
3. **`indexerService.ts` is control-plane only** — it never reads the chain; it
   manages the shared cursor and triggers replays.

### The Dual-Indexer Race

Both `SorobanEventWorker` and `SorobanIndexerService` poll the same Soroban RPC
for the same contract events and write to the same `Stream` and `StreamEvent`
rows. Because they run on independent timers, they can race:

- Both may process the same ledger simultaneously.
- Both write to the same `Stream` row (upsert), so the last writer wins —
  usually harmless for immutable fields but problematic for additive mutations
  like `withdrawnAmount` (issue #808).
- `StreamEvent` dedup via `@@unique([transactionHash, eventType])` prevents
  duplicate event rows, but does **not** protect stream state mutations.

**Mitigation:** Do not extend the legacy indexer. The consolidation (issue #801)
will remove `SorobanIndexerService` entirely.

### Naming Convention Plan

The team convention is kebab-case with a `.service.ts` suffix. Once functional
consolidation lands:

| Current Name | Expected Future Name |
|---|---|
| `indexerService.ts` | `indexer.service.ts` |
| `soroban-indexer.service.ts` | *(removed)* |

---

## SSE Broadcast Flow

The SSE (Server-Sent Events) subsystem delivers real-time contract event
notifications to connected frontend clients. The full SSE architecture (scaling,
memory, security) is documented in
[`docs/SSE_ARCHITECTURE.md`](./SSE_ARCHITECTURE.md).

### End-to-End Path

```
Soroban RPC
    │  SorobanEventWorker polls for new contract events
    ▼
SorobanEventWorker (src/workers/soroban-event-worker.ts)
    │  decode XDR → upsert Stream → insert StreamEvent
    ▼
PostgreSQL (via Prisma)
    │  Stream + StreamEvent rows updated
    ▼
SSE broadcast (src/services/sse.service.ts)
    │  sseService.broadcastToStream(streamId, event, data)
    │  sseService.broadcastToUser(publicKey, event, data)
    │  sseService.broadcastToAdmin(event, data)
    │
    ├──► [Single instance]  Direct write to in-memory client registry
    │
    └──► [Multi-instance]   Redis Pub/Sub
                             │  publish to sse:stream:<id>, sse:user:<address>
                             ▼
                             All backend instances subscribe
                             │  rebroadcast to local connected clients
                             ▼
                         Frontend (useStreamEvents hook)
```

### Broadcast Channels

The worker uses three broadcast entry points depending on the event:

| Method | When Used | Target Audience |
|--------|-----------|-----------------|
| `sseService.broadcastToStream(streamId, event, data)` | Stream lifecycle events (created, topped_up, withdrawn, cancelled, completed, paused, resumed) | Clients subscribed to that specific stream ID or `*` |
| `sseService.broadcastToUser(publicKey, event, data)` | (Reserved for user-scoped events) | Clients subscribed to `user:<publicKey>` or `*` |
| `sseService.broadcastToAdmin(event, data)` | Protocol-level events (fee_collected, fee_config_updated, admin_transferred) | The admin user identified by `ADMIN_PUBLIC_KEY` env var |

### Multi-Instance Fanout

When `REDIS_URL` is configured, broadcasts go through Redis Pub/Sub instead of
direct in-memory writes:

1. The originating instance publishes `{ event, data }` to
   `sse:stream:<id>` or `sse:user:<address>`.
2. Every backend instance subscribes via `psubscribe('sse:stream:*',
   'sse:user:*')` and rebroadcasts to its own local clients.
3. This means events reach all connected clients regardless of which backend
   instance they are connected to.

### Client Connection Limits

| Limit | Default | Env Var |
|-------|---------|---------|
| Max SSE connections per server | 10,000 | `MAX_SSE_CONNECTIONS` |
| Max connections per IP | 5 | Hardcoded |
| Max connections per authenticated user | 10 | Hardcoded |

Slow clients (write buffer ≥ 64 KB) are automatically dropped to protect
throughput for healthy clients.

---

## Keeper-Key Authorization Model

FlowFi splits transaction signing into two categories: custodial (server-signed)
and non-custodial (wallet-signed). The signing key determines who is responsible
for the transaction.

### Action Signing Matrix

| Action | Signer | Mechanism |
|--------|--------|-----------|
| **Top-up** | Server (custodial) | Backend submits the transaction using `KEEPER_SECRET_KEY`. The frontend sends only the stream ID and amount. |
| **Withdraw** | Wallet (non-custodial) | Frontend builds and signs the transaction via the connected wallet (Freighter). The backend simulate endpoint exists for fee estimation only. |
| **Pause / Resume** | Wallet (non-custodial) | Same as withdraw — frontend-signed. Backend simulate endpoints exist for fee estimation but do not submit. |
| **Create stream** | Wallet (non-custodial) | Frontend signs via wallet and submits directly to the Soroban RPC. |

### The `KEEPER_SECRET_KEY`

- Stored as an environment variable on the backend.
- Loaded by `src/services/sorobanService.ts` via
  `process.env.KEEPER_SECRET_KEY`.
- Used **exclusively** by the top-up flow. The `topUpStream` function builds
  the transaction, signs it with the keeper keypair, and submits it to the
  Soroban RPC.
- If `KEEPER_SECRET_KEY` is not configured, `topUpStream` throws
  `'KEEPER_SECRET_KEY not configured'` and the request returns HTTP 500.
- The cancel endpoint (`src/controllers/stream/cancel.ts`) also reads
  `KEEPER_SECRET_KEY` but only to check whether the server wallet is configured;
  the actual cancel transaction is wallet-signed by the sender.

### Security Boundary

> **Do not wire pause/resume/withdraw to a server-side submit path.** Only
> `top-up` is intentionally custodial. All other mutating actions must be
> wallet-signed by the user to preserve the non-custodial security model.

The keeper key is a server-side secret and is never exposed to the frontend.

---

## Payroll Provider Adapter Contract

Payroll integrations (e.g. an external payroll provider) map **approved** payroll
batches to FlowFi payment instructions. The adapter is a pure translation layer:
it validates the batch, produces unsigned payment instructions, and reports a
per-employee result. It **never signs** — final signing stays with the authorized
wallet (see the Keeper-Key Authorization Model above).

### Interface

```ts
export interface PayrollProviderAdapter {
  readonly providerId: string;
  /** Convert an approved batch into unsigned FlowFi payment instructions. */
  buildInstructions(
    batch: ApprovedPayrollBatch,
    options: BuildInstructionsOptions,
  ): Promise<PayrollInstructionResult>;
}
```

`buildInstructions` returns `PayrollInstructionResult` containing the produced
`instructions` (unsigned) and a `results` array with one entry per employee.

### Idempotency

- Every call requires an `idempotencyKey` in `BuildInstructionsOptions`.
- Instructions are deduped on `(providerId, idempotencyKey, batchId, employeeId)`.
  Replaying the same key returns the previously produced instructions instead of
  creating duplicates.
- A batch may only be processed once it is in the `approved` state; re-submitting
  an already-processed batch with a new key is rejected as a validation error.

### Validation Rules

Before any instruction is produced the adapter validates:

- **Batch:** present, `status === 'approved'`, non-empty employee list, unique
  `employeeId` per batch, and a supported `network`/`chain` (must match the
  configured FlowFi network).
- **Employee:** valid Stellar public key (`G...`), positive `amount` with at most
  7 decimal places (Stellar precision), and a supported asset.
- **Amounts:** each amount must be > 0 and the batch total must not exceed the
  configured per-batch limit.

Validation failures are reported per employee in `results` (with a `reason`);
batch-level failures reject the whole call with a typed error.

### Per-Employee Result Mapping

Each employee in the batch maps to exactly one result entry:

| Status | Meaning |
|--------|---------|
| `success` | Instruction produced for this employee. |
| `failed` | Employee failed validation; `reason` explains why. |
| `skipped` | Employee intentionally not paid (e.g. zero amount, opted out); `reason` set. |

### Security Boundary

> **The payroll adapter must never sign or submit transactions.** It only emits
> unsigned payment instructions. Signing and submission remain with the
> authorized wallet, preserving the non-custodial security model. The adapter
> must not read `KEEPER_SECRET_KEY` or any signing secret.
