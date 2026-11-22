import type {
  BackendStream,
  RecurringSchedule,
  RecurringScheduleCreateRequest,
  ScheduleFundingPreview,
  ScheduleRun,
} from "@/lib/api-types";
import { TOKEN_ADDRESSES } from "@/lib/soroban";
import { shortenPublicKey } from "@/lib/wallet";
import {
  DEFAULT_FETCH_TIMEOUT_MS,
  fetchWithTimeout,
  getStreamsEndpointCandidates,
  toTokenAmount,
} from "@/lib/api/_shared";

/**
 * Stable error code returned by the backend when a mutation is rejected
 * because the stream changed after the client reviewed it.
 */
export const STREAM_VERSION_CONFLICT_CODE = "STREAM_VERSION_CONFLICT" as const;

/**
 * Payload returned alongside a 409 response for stale stream mutations.
 * `currentVersion` is the version the client must re-read before retrying.
 */
export interface StreamVersionConflict {
  code: typeof STREAM_VERSION_CONFLICT_CODE;
  message: string;
  streamId: number;
  expectedVersion: number;
  currentVersion: number;
  currentLastUpdateTime?: number;
}

/**
 * Thrown by mutation helpers when the backend reports a version conflict.
 * Callers can catch this to refresh the stream and re-prompt the user.
 */
export class StreamVersionConflictError extends Error {
  readonly conflict: StreamVersionConflict;

  constructor(conflict: StreamVersionConflict) {
    super(conflict.message);
    this.name = "StreamVersionConflictError";
    this.conflict = conflict;
  }
}

export type ReadinessSeverity = "blocker" | "warning";

export interface ReadinessCheck {
  code: string;
  severity: ReadinessSeverity;
  message: string;
}

export interface StreamReadiness {
  streamId: number;
  action: string;
  ready: boolean;
  version: number;
  checkedAt: number;
  blockers: ReadinessCheck[];
  warnings: ReadinessCheck[];
}

export type IncomingStreamStatus = "Active" | "Paused" | "Completed";

export interface IncomingStreamRecord {
  id: string;
  streamId: number;
  sender: string;
  senderDisplay: string;
  token: string;
  tokenAddress: string;
  ratePerSecond: number;
  deposited: number;
  withdrawn: number;
  startTime: number;
  lastUpdateTime: number;
  isActive: boolean;
  isPaused: boolean;
  pausedAt: number | null;
  totalPausedDuration: number;
  status: IncomingStreamStatus;
}

interface StreamListResponse {
  data?: BackendStream[];
}

function resolveTokenLabel(tokenAddress: string): string {
  const entry = Object.entries(TOKEN_ADDRESSES).find(
    ([, address]) => address === tokenAddress,
  );

  return entry?.[0] ?? `${tokenAddress.slice(0, 6)}...${tokenAddress.slice(-4)}`;
}

function toIncomingStreamStatus(stream: BackendStream): IncomingStreamStatus {
  if (stream.isPaused) return "Paused";
  if (stream.isActive) return "Active";
  return "Completed";
}

function mapBackendStream(stream: BackendStream): IncomingStreamRecord {
  return {
    id: stream.id,
    streamId: stream.streamId,
    sender: stream.sender,
    senderDisplay: shortenPublicKey(stream.sender),
    token: resolveTokenLabel(stream.tokenAddress),
    tokenAddress: stream.tokenAddress,
    ratePerSecond: toTokenAmount(stream.ratePerSecond),
    deposited: toTokenAmount(stream.depositedAmount),
    withdrawn: toTokenAmount(stream.withdrawnAmount),
    startTime: stream.startTime,
    lastUpdateTime: stream.lastUpdateTime,
    isActive: stream.isActive,
    isPaused: stream.isPaused ?? false,
    pausedAt: stream.pausedAt ?? null,
    totalPausedDuration: stream.totalPausedDuration ?? 0,
    status: toIncomingStreamStatus(stream),
  };
}

function isVersionConflictPayload(value: unknown): value is StreamVersionConflict {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<StreamVersionConflict>;
  return (
    candidate.code === STREAM_VERSION_CONFLICT_CODE &&
    typeof candidate.streamId === "number" &&
    typeof candidate.expectedVersion === "number" &&
    typeof candidate.currentVersion === "number"
  );
}

async function parseVersionConflict(
  response: Response,
  fallback: { streamId: number; expectedVersion: number },
): Promise<StreamVersionConflictError> {
  let payload: unknown = null;
  try {
    payload = await response.json();
  } catch {
    payload = null;
  }

  const conflict: StreamVersionConflict = isVersionConflictPayload(payload)
    ? payload
    : {
        code: STREAM_VERSION_CONFLICT_CODE,
        message: "Stream changed since it was reviewed. Refresh and retry.",
        streamId: fallback.streamId,
        expectedVersion: fallback.expectedVersion,
        currentVersion: -1,
      };

  return new StreamVersionConflictError(conflict);
}

export interface StreamMutationOptions {
  /** Version the client reviewed; sent as `expectedVersion` for optimistic concurrency. */
  expectedVersion: number;
  authToken?: string | null;
  timeoutMs?: number;
}

/**
 * Submit a stream mutation with optimistic concurrency control.
 *
 * The wallet-signed transaction is still required and validated server-side;
 * `expectedVersion` only guards against acting on stale state. A 409 response
 * is surfaced as a {@link StreamVersionConflictError} carrying the current
 * version metadata so callers can refresh and retry.
 */
export async function submitStreamMutation(
  streamId: number,
  action: string,
  signedTransactionXdr: string,
  options: StreamMutationOptions,
): Promise<unknown> {
  const endpoints = getStreamsEndpointCandidates();
  const endpoint = endpoints[0];
  const timeoutMs = options.timeoutMs ?? DEFAULT_FETCH_TIMEOUT_MS;

  const response = await fetchWithTimeout(
    `${endpoint}/${streamId}/${action}`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(options.authToken
          ? { Authorization: `Bearer ${options.authToken}` }
          : {}),
      },
      body: JSON.stringify({
        signedTransactionXdr,
        expectedVersion: options.expectedVersion,
      }),
    },
    timeoutMs,
  );

  if (response.status === 409) {
    throw await parseVersionConflict(response, {
      streamId,
      expectedVersion: options.expectedVersion,
    });
  }

  if (!response.ok) {
    throw new Error(
      `Stream mutation failed (${response.status}) for ${action} on ${streamId}`,
    );
  }

  return response.json();
}

/**
 * Fetch machine-readable preflight readiness for a stream action.
 *
 * Results are advisory only: `ready === true` does not guarantee the
 * transaction will succeed, since state can change between the check and the
 * signed submission. Blockers must be resolved; warnings are informational.
 */
export async function fetchStreamReadiness(
  streamId: number,
  action: string,
  options: { authToken?: string | null; timeoutMs?: number } = {},
): Promise<StreamReadiness> {
  const endpoints = getStreamsEndpointCandidates();
  const endpoint = endpoints[0];
  const timeoutMs = options.timeoutMs ?? DEFAULT_FETCH_TIMEOUT_MS;
  const params = new URLSearchParams({ action });

  const response = await fetchWithTimeout(
    `${endpoint}/${streamId}/readiness?${params.toString()}`,
    { headers: options.authToken ? { Authorization: `Bearer ${options.authToken}` } : {} },
    timeoutMs,
  );

  if (!response.ok) {
    throw new Error(
      `Failed to fetch readiness (${response.status}) for ${action} on ${streamId}`,
    );
  }

  const payload = (await response.json()) as Partial<StreamReadiness>;
  return {
    streamId: payload.streamId ?? streamId,
    action: payload.action ?? action,
    ready: payload.ready ?? false,
    version: payload.version ?? -1,
    checkedAt: payload.checkedAt ?? Date.now(),
    blockers: payload.blockers ?? [],
    warnings: payload.warnings ?? [],
  };
}

export async function fetchIncomingStreams(
  recipientPublicKey: string,
  timeoutMs: number = DEFAULT_FETCH_TIMEOUT_MS,
  authToken?: string | null,
): Promise<IncomingStreamRecord[]> {
  const endpoints = getStreamsEndpointCandidates();
  const params = new URLSearchParams({
    recipient: recipientPublicKey,
    sort: "lastUpdateTime",
    order: "desc",
    limit: "100",
  });
  let lastError: Error | null = null;

  for (const endpoint of endpoints) {
    const response = await fetchWithTimeout(
      `${endpoint}?${params.toString()}`,
      { headers: authToken ? { Authorization: `Bearer ${authToken}` } : {} },
      timeoutMs,
    );

    if (response.ok) {
      const payload = (await response.json()) as BackendStream[] | StreamListResponse;
      const streams = Array.isArray(payload) ? payload : payload.data ?? [];
      return streams.map(mapBackendStream);
    }

    if (response.status === 404) {
      lastError = new Error(`Endpoint not found: ${endpoint}`);
      continue;
    }

    lastError = new Error(
      `Failed to fetch incoming streams (${response.status}) from ${endpoint}`,
    );
  }

  throw lastError ?? new Error("Failed to fetch incoming streams from backend.");
}

// ---------------------------------------------------------------------------
// Recurring payment schedules
// ---------------------------------------------------------------------------

export interface RecurringScheduleRecord {
  id: string;
  scheduleId: number;
  sender: string;
  senderDisplay: string;
  recipient: string;
  recipientDisplay: string;
  token: string;
  tokenAddress: string;
  amountPerRun: number;
  intervalSeconds: number;
  nextRunAt: number;
  remainingRuns: number;
  status: RecurringSchedule["status"];
  /** Each generated stream is tracked as its own payment obligation. */
  runs: ScheduleRun[];
  createdAt: number;
  updatedAt: number;
}

export interface ScheduleFundingPreviewRecord {
  scheduleId: number;
  token: string;
  tokenAddress: string;
  requiredAmount: number;
  availableBalance: number;
  shortfall: number;
  sufficient: boolean;
  nextRunAt: number;
  checkedAt: number;
}

interface ScheduleListResponse {
  data?: RecurringSchedule[];
}

interface ScheduleFundingPreviewResponse {
  data?: ScheduleFundingPreview;
}

function resolveScheduleEndpointCandidates(): string[] {
  return getStreamsEndpointCandidates().map((endpoint) =>
    endpoint.replace(/\/streams\/?$/, "/schedules"),
  );
}

function mapScheduleRun(run: ScheduleRun): ScheduleRun {
  return {
    ...run,
    amount: toTokenAmount(run.amount),
  };
}

function mapRecurringSchedule(schedule: RecurringSchedule): RecurringScheduleRecord {
  return {
    id: schedule.id,
    scheduleId: schedule.scheduleId,
    sender: schedule.sender,
    senderDisplay: shortenPublicKey(schedule.sender),
    recipient: schedule.recipient,
    recipientDisplay: shortenPublicKey(schedule.recipient),
    token: resolveTokenLabel(schedule.tokenAddress),
    tokenAddress: schedule.tokenAddress,
    amountPerRun: toTokenAmount(schedule.amountPerRun),
    intervalSeconds: schedule.intervalSeconds,
    nextRunAt: schedule.nextRunAt,
    remainingRuns: schedule.remainingRuns,
    status: schedule.status,
    runs: (schedule.runs ?? []).map(mapScheduleRun),
    createdAt: schedule.createdAt,
    updatedAt: schedule.updatedAt,
  };
}

function mapFundingPreview(
  preview: ScheduleFundingPreview,
): ScheduleFundingPreviewRecord {
  return {
    scheduleId: preview.scheduleId,
    token: resolveTokenLabel(preview.tokenAddress),
    tokenAddress: preview.tokenAddress,
    requiredAmount: toTokenAmount(preview.requiredAmount),
    availableBalance: toTokenAmount(preview.availableBalance),
    shortfall: toTokenAmount(preview.shortfall),
    sufficient: preview.sufficient,
    nextRunAt: preview.nextRunAt,
    checkedAt: preview.checkedAt,
  };
}

async function requestScheduleEndpoint<T>(
  path: string,
  init: RequestInit,
  timeoutMs: number,
  authToken?: string | null,
): Promise<T> {
  const endpoints = resolveScheduleEndpointCandidates();
  let lastError: Error | null = null;

  for (const endpoint of endpoints) {
    const response = await fetchWithTimeout(
      `${endpoint}${path}`,
      {
        ...init,
        headers: {
          ...(init.body ? { "Content-Type": "application/json" } : {}),
          ...(authToken ? { Authorization: `Bearer ${authToken}` } : {}),
          ...(init.headers ?? {}),
        },
      },
      timeoutMs,
    );

    if (response.ok) {
      return (await response.json()) as T;
    }

    if (response.status === 404) {
      lastError = new Error(`Endpoint not found: ${endpoint}${path}`);
      continue;
    }

    lastError = new Error(
      `Schedule request failed (${response.status}) from ${endpoint}${path}`,
    );
  }

  throw lastError ?? new Error("Failed to reach schedule backend.");
}

export async function fetchRecurringSchedules(
  ownerPublicKey: string,
  timeoutMs: number = DEFAULT_FETCH_TIMEOUT_MS,
  authToken?: string | null,
): Promise<RecurringScheduleRecord[]> {
  const params = new URLSearchParams({
    owner: ownerPublicKey,
    sort: "nextRunAt",
    order: "asc",
    limit: "100",
  });
  const payload = await requestScheduleEndpoint<
    RecurringSchedule[] | ScheduleListResponse
  >(`?${params.toString()}`, { method: "GET" }, timeoutMs, authToken);
  const schedules = Array.isArray(payload) ? payload : payload.data ?? [];
  return schedules.map(mapRecurringSchedule);
}

export async function createRecurringSchedule(
  request: RecurringScheduleCreateRequest,
  timeoutMs: number = DEFAULT_FETCH_TIMEOUT_MS,
  authToken?: string | null,
): Promise<RecurringScheduleRecord> {
  const payload = await requestScheduleEndpoint<RecurringSchedule>(
    "",
    { method: "POST", body: JSON.stringify(request) },
    timeoutMs,
    authToken,
  );
  return mapRecurringSchedule(payload);
}

export async function pauseRecurringSchedule(
  scheduleId: number,
  timeoutMs: number = DEFAULT_FETCH_TIMEOUT_MS,
  authToken?: string | null,
): Promise<RecurringScheduleRecord> {
  const payload = await requestScheduleEndpoint<RecurringSchedule>(
    `/${scheduleId}/pause`,
    { method: "POST" },
    timeoutMs,
    authToken,
  );
  return mapRecurringSchedule(payload);
}

export async function cancelRecurringSchedule(
  scheduleId: number,
  timeoutMs: number = DEFAULT_FETCH_TIMEOUT_MS,
  authToken?: string | null,
): Promise<RecurringScheduleRecord> {
  const payload = await requestScheduleEndpoint<RecurringSchedule>(
    `/${scheduleId}/cancel`,
    { method: "POST" },
    timeoutMs,
    authToken,
  );
  return mapRecurringSchedule(payload);
}

export async function fetchScheduleFundingPreview(
  scheduleId: number,
  timeoutMs: number = DEFAULT_FETCH_TIMEOUT_MS,
  authToken?: string | null,
): Promise<ScheduleFundingPreviewRecord> {
  const payload = await requestScheduleEndpoint<
    ScheduleFundingPreview | ScheduleFundingPreviewResponse
  >(`/${scheduleId}/funding-preview`, { method: "GET" }, timeoutMs, authToken);
  const preview = "data" in payload && payload.data ? payload.data : payload;
  return mapFundingPreview(preview as ScheduleFundingPreview);
}
