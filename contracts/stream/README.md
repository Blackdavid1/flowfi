# Stream Contract

The stream contract lets a funder create a stream that pays a recipient over time.
Each stream is independently funded, uniquely identified, and independently
cancellable.

## Streams

- `create_stream(funder, recipient, amount, start, duration)` — creates a single
  stream instance. The funder must authorize the call and have sufficient
  balance; the stream is identified by a unique stream id.
- `cancel_stream(stream_id)` — cancels a single stream instance. Only the funder
  (or an authorized party) may cancel, and only that instance is affected.
- `withdraw(stream_id)` — withdraws the vested amount for a single instance.

## Recurring schedules

A recurring schedule spawns successive stream instances from a bounded
schedule. Each generated instance is a normal stream: independently funded,
uniquely identified, and independently cancellable. Cancelling one instance
never affects its siblings.

### Schedule parameters

A schedule is defined by:

| Field        | Meaning                                                        |
| ------------ | -------------------------------------------------------------- |
| `interval`   | Seconds between successive instance start times.               |
| `start`      | Start time of the first instance.                              |
| `count`      | Maximum number of instances the schedule may create.           |
| `amount`     | Amount funded per instance.                                    |
| `duration`   | Duration of each instance.                                     |

### Limits

Schedules are bounded so that a single schedule cannot create unbounded work:

- `interval` must be within `[MIN_INTERVAL, MAX_INTERVAL]`.
- `count` must be within `[1, MAX_INSTANCES]`.
- `amount` must be greater than zero.

Schedules outside these bounds are rejected at creation time.

### Deterministic linkage

Each instance created by a schedule is deterministically linked to its parent
via `(schedule_id, index)`, where `index` is the zero-based position of the
instance in the schedule. The instance id is derived from this pair, so the same
schedule and index always map to the same instance id. This makes duplicate
execution idempotent: attempting to create an instance that already exists is
rejected rather than creating a second stream.

### Missed intervals

If execution is delayed, missed intervals are **skipped**, not caught up. Only
the instance for the current interval index is created; earlier missed indices
are not retroactively created. This keeps accounting deterministic and avoids
unexpected bulk funding.

### Insufficient funding

Each instance is funded independently. If the funder lacks sufficient balance
for the next instance, creation of that instance fails and the schedule does not
advance. Previously created instances are unaffected.

## Rate and duration ceilings

Protocol policy may bound the per-stream rate and duration so that accidental or
abusive configurations cannot be created. The ceilings are configurable by the
policy owner and are enforced only on the stream creation path.

### Units

| Policy field     | Unit                                                          |
| ---------------- | ------------------------------------------------------------- |
| `max_rate`       | Token base units per second (rate = `amount / duration`).     |
| `max_duration`   | Seconds.                                                      |

A stream's rate is derived from its `amount` and `duration`; a stream is
rejected when its derived rate exceeds `max_rate` or its `duration` exceeds
`max_duration`. Configurations exactly at a ceiling are accepted; only values
strictly above a ceiling are rejected.

### Policy-change behavior

Ceilings apply **only to newly created streams**. Streams that already exist
retain the terms they were created with, even if a later policy change would
make those terms exceed the new ceilings. Lowering a ceiling never mutates,
cancels, or truncates existing streams; it only prevents future creations that
exceed the new bounds.

### Errors

Creation that exceeds either ceiling is rejected with a clear contract error
(`RateExceedsCeiling` / `DurationExceedsCeiling`) rather than silently clamping
the configuration.

## Testing

Focused coverage for recurring schedules includes:

- creating successive instances from a valid schedule;
- rejecting schedules with out-of-bounds `interval` or `count`;
- missed intervals being skipped deterministically;
- insufficient funding for an instance failing without advancing the schedule;
- duplicate execution being rejected (deterministic linkage);
- each instance remaining independently cancellable.

Focused coverage for rate and duration ceilings includes:

- accepting a stream exactly at `max_rate` and exactly at `max_duration`;
- rejecting a stream just above `max_rate` or just above `max_duration`;
- accepting a stream just below each ceiling;
- demonstrating that existing streams retain their agreed terms after a policy
  change lowers the ceilings.
