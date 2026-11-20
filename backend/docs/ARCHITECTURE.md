# Backend Architecture

This document is the canonical reference for backend service architecture. It is
referenced by `backend/src/services/indexerService.ts` and
`backend/src/services/soroban-indexer.service.ts` as the authoritative source for
indexer ownership, SSE broadcast flow, and keeper-key authorization.

For the full project-wide architecture (event type data flows, pause/resume
timing, environment variables, and operational runbook) see
[`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md).

---

## Financial Language & Risk-Disclosure Glossary

This glossary is the canonical definition of the financial terms surfaced in the
UI and API. Every user-facing label, API field description, and docs page must
use these terms with the meanings below. When a value is derived rather than
observed on-chain, the caveat column is mandatory disclosure.

| Term | Definition | Authoritative Source | Caveats / Risk Disclosure |
|------|------------|----------------------|---------------------------|
| **Streamed** | The portion of a stream's total amount that has vested by elapsed time up to the current ledger timestamp. | Contract `streamed_amount` view / `SorobanEventWorker` recomputation from `startTime`, `ratePerSecond`, and ledger time. | Time-based estimate between ledgers; not a settled balance. Do not present as withdrawable. |
| **Claimable** | The amount the recipient can withdraw right now: streamed amount minus already-withdrawn amount, clamped to the stream's remaining balance. | Contract `claimable_amount` view; mirrored by backend `withdrawnAmount` accounting. | Depends on the latest indexed ledger; may lag chain state by one poll interval. Never treat as guaranteed until a withdraw transaction confirms. |
| **Deposited** | The total amount the sender has funded into the stream (initial deposit plus any top-ups). | Contract `deposited_amount`; backend `Stream.depositedAmount` written by the indexer on `created` / `topped_up` events. | Custodial top-ups are server-signed via `KEEPER_SECRET_KEY`; a pending top-up is not yet deposited until the transaction is confirmed and indexed. |
| **Withdrawn** | The cumulative amount the recipient has successfully claimed from the stream. | Contract `withdrawn_amount`; backend `Stream.withdrawnAmount` updated on `withdrawn` events. | Additive mutation — the dual-indexer race (issue #808) can cause last-writer-wins drift until consolidation (issue #801). Treat as eventually consistent. |
| **Estimated** | Any value computed client- or server-side from time math rather than read from a confirmed on-chain state. | Frontend `useStreamEvents` projections; backend simulate endpoints (fee estimation only). | Must be labeled "estimated" in the UI. Never used for accounting, authorization, or payout decisions. |
| **Finalized** | A value backed by a confirmed, indexed on-chain transaction/event. | `StreamEvent` rows with a confirmed `transactionHash`; `Stream` fields written by `SorobanEventWorker`. | Finalized values can still be superseded by later events (e.g. a subsequent withdraw), but are never rolled back by estimation. |
| **Stale** | A displayed value whose backing indexer cursor has not advanced within the expected poll window, so it may no longer reflect chain state. | `IndexerState` cursor freshness vs. `SorobanEventWorker` poll interval; SSE heartbeat gaps. | Stale values must be visibly flagged in the UI and must not be treated as finalized. Recovery is via `replayFromLedger` (see `indexerService.ts`). |

### Consistency Rules

1. **Estimated vs. finalized.** Any value derived from time math is
   *estimated*; only values written from confirmed `StreamEvent` rows are
   *finalized*. UI must not render an estimated value with finalized styling.
2. **Stale vs. finalized.** If the `IndexerState` cursor is behind the expected
   poll window, all derived values are *stale* until the worker catches up. Stale
   is a superset of estimated: a stale finalized value is still stale.
3. **Claimable ≤ streamed − withdrawn.** This invariant must hold in every
   surface; a violation indicates indexer drift, not a UI bug.
4. **Deposited is monotonic.** Deposited only increases (deposit + top-ups); a
   decrease signals a data-integrity failure and must be surfaced, not hidden.

### Key UI & API Surfaces

These surfaces must use the glossary terms verbatim and honor the consistency
rules above:

| Surface | Terms Exposed | Notes |
|---------|---------------|-------|
| `GET /api/streams/:id` | streamed, claimable, deposited, withdrawn | Response fields map 1:1 to glossary terms; estimated fields are flagged. |
| `GET /api/streams` (list) | streamed, claimable | List projections are estimated between polls. |
| SSE `stream` events (`sse.service.ts`) | withdrawn, deposited, finalized | Event payloads are finalized once broadcast from the worker. |
| `GET /api/indexer/status` (`indexerService.ts`) | stale | Reports cursor freshness used to compute staleness. |
| Frontend `useStreamEvents` hook | estimated, stale | Client projections must be labeled estimated and flagged stale on heartbeat gaps. |

### Automated Coverage

Focused tests assert the glossary invariants and boundary transitions:

- **Stale → finalized transition:** a value flagged stale becomes finalized once
  the indexer cursor advances past the event ledger.
- **Estimated → finalized transition:** an estimated claimable value is replaced
  by the finalized on-chain value after the `withdrawn` event is indexed.
- **Boundary:** claimable clamps to zero when withdrawn equals streamed, and to
  the remaining balance when a top-up lands mid-stream.
- **Failure:** a deposited decrease is rejected/surfaced rather than silently
  applied.

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
