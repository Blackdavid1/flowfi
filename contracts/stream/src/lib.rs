//! Recurring stream contract.
//!
//! Streams can be created directly (single instance) or from a bounded
//! recurring schedule. Each instance produced by a schedule is independently
//! funded, uniquely identified, and independently cancellable. Linkage between
//! instances is deterministic: `(schedule_id, index)`.
//!
//! ## Sponsorship
//!
//! A stream may optionally be created with an explicit `sponsor`. The sponsor
//! is a distinct role from `owner` (sender) and `recipient`: it may cover the
//! transaction costs and/or initial funding of a stream, but it never gains
//! sender or recipient privileges. Sponsorship is recorded in state and emitted
//! in events separately from the sender/recipient so attribution is explicit.
//! When no sponsor is supplied the stream is unsponsored and behaves exactly as
//! before.
//!
//! ## Rate and duration ceilings
//!
//! Protocol policy may bound per-stream configuration via two ceilings:
//!
//! - `max_rate`: maximum stream rate in token base units **per second**
//!   (`amount / interval`, integer division).
//! - `max_duration`: maximum stream duration in **seconds** (`interval * count`).
//!
//! A ceiling of `0` means "unbounded" for that dimension. Ceilings are applied
//! only at creation time: streams and schedules created before a policy change
//! retain their agreed terms, and later policy changes never retroactively
//! reject or alter existing streams.

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Vec};

/// Bounds enforced on recurring schedules.
pub const MIN_INTERVAL: u64 = 60;
pub const MAX_INTERVAL: u64 = 31_536_000; // one year
pub const MAX_INSTANCES: u32 = 52;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stream {
    pub id: u64,
    pub owner: Address,
    pub recipient: Address,
    /// Optional sponsor that covered costs/funding. Distinct from `owner` and
    /// `recipient`; carries no sender or recipient privileges.
    pub sponsor: Option<Address>,
    pub amount: i128,
    pub start: u64,
    pub cancelled: bool,
}

/// A bounded recurring schedule. `count` is the maximum number of instances
/// that may ever be spawned from this schedule.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Schedule {
    pub id: u64,
    pub owner: Address,
    pub recipient: Address,
    /// Optional sponsor inherited by spawned instances. Distinct from `owner`
    /// and `recipient`; carries no sender or recipient privileges.
    pub sponsor: Option<Address>,
    pub amount: i128,
    pub start: u64,
    pub interval: u64,
    pub count: u32,
    pub spawned: u32,
}

/// Configurable protocol policy ceilings. `0` disables a ceiling.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    /// Max rate in token base units per second (`amount / interval`).
    pub max_rate: i128,
    /// Max duration in seconds (`interval * count`).
    pub max_duration: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidInterval = 1,
    InvalidCount = 2,
    InvalidAmount = 3,
    ScheduleNotFound = 4,
    ScheduleExhausted = 5,
    InsufficientFunding = 6,
    DuplicateExecution = 7,
    StreamNotFound = 8,
    Unauthorized = 9,
    RateExceedsCeiling = 10,
    DurationExceedsCeiling = 11,
    InvalidSponsor = 12,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataKey {
    pub kind: u32,
    pub id: u64,
}

const KEY_STREAM: u32 = 0;
const KEY_SCHEDULE: u32 = 1;
const KEY_NEXT_STREAM: u32 = 2;
const KEY_NEXT_SCHEDULE: u32 = 3;
const KEY_POLICY: u32 = 4;

#[contract]
pub struct StreamContract;

#[contractimpl]
impl StreamContract {
    /// Create a single stream instance directly.
    ///
    /// `owner` (sender) must authorize. An optional `sponsor` may be supplied to
    /// cover costs/funding; the sponsor is recorded for attribution only and
    /// never gains sender or recipient privileges. A sponsor equal to the
    /// recipient is rejected so sponsorship cannot be used to smuggle
    /// recipient authority.
    pub fn create_stream(
        env: Env,
        owner: Address,
        recipient: Address,
        sponsor: Option<Address>,
        amount: i128,
        start: u64,
    ) -> Result<u64, Error> {
        owner.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if let Some(ref s) = sponsor {
            if *s == recipient {
                return Err(Error::InvalidSponsor);
            }
        }
        let id = next_id(&env, KEY_NEXT_STREAM);
        let stream = Stream {
            id,
            owner,
            recipient,
            sponsor,
            amount,
            start,
            cancelled: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey { kind: KEY_STREAM, id }, &stream);
        Ok(id)
    }

    /// Create a bounded recurring schedule. Rejects out-of-bounds intervals and
    /// instance counts so a schedule can never spawn unbounded instances.
    /// Also enforces the current rate and duration ceilings (see module docs).
    ///
    /// An optional `sponsor` is recorded for attribution and inherited by
    /// spawned instances; it never gains sender or recipient privileges.
    pub fn create_schedule(
        env: Env,
        owner: Address,
        recipient: Address,
        sponsor: Option<Address>,
        amount: i128,
        start: u64,
        interval: u64,
        count: u32,
    ) -> Result<u64, Error> {
        owner.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if interval < MIN_INTERVAL || interval > MAX_INTERVAL {
            return Err(Error::InvalidInterval);
        }
        if count == 0 || count > MAX_INSTANCES {
            return Err(Error::InvalidCount);
        }
        if let Some(ref s) = sponsor {
            if *s == recipient {
                return Err(Error::InvalidSponsor);
            }
        }
        enforce_ceilings(&env, amount, interval, count)?;
        let id = next_id(&env, KEY_NEXT_SCHEDULE);
        let schedule = Schedule {
            id,
            owner,
            recipient,
            sponsor,
            amount,
            start,
            interval,
            count,
            spawned: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey { kind: KEY_SCHEDULE, id }, &schedule);
        Ok(id)
    }

    /// Spawn the next stream instance from a schedule.
    ///
    /// Missed intervals are skipped deterministically: the next instance is
    /// always anchored at `start + spawned * interval`, so a late call never
    /// double-spawns an interval and never shifts the schedule. Each spawned
    /// instance is independently funded (the caller must supply `funding`
    /// covering `amount`) and independently cancellable. The schedule's sponsor
    /// (if any) is carried onto the instance for attribution only.
    pub fn spawn_from_schedule(
        env: Env,
        schedule_id: u64,
        funding: i128,
    ) -> Result<u64, Error> {
        let key = DataKey {
            kind: KEY_SCHEDULE,
            id: schedule_id,
        };
        let mut schedule: Schedule = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::ScheduleNotFound)?;

        schedule.owner.require_auth();

        if schedule.spawned >= schedule.count {
            return Err(Error::ScheduleExhausted);
        }
        if funding < schedule.amount {
            return Err(Error::InsufficientFunding);
        }

        let index = schedule.spawned;
        let start = schedule
            .start
            .checked_add((index as u64).checked_mul(schedule.interval).ok_or(Error::InvalidInterval)?)
            .ok_or(Error::InvalidInterval)?;

        // Deterministic linkage: stream id derived from schedule id + index.
        let stream_id = schedule_id
            .checked_mul(1_000_000)
            .and_then(|v| v.checked_add(index as u64))
            .ok_or(Error::DuplicateExecution)?;
        let stream_key = DataKey {
            kind: KEY_STREAM,
            id: stream_id,
        };
        if env.storage().persistent().has(&stream_key) {
            return Err(Error::DuplicateExecution);
        }

        let stream = Stream {
            id: stream_id,
            owner: schedule.owner.clone(),
            recipient: schedule.recipient.clone(),
            sponsor: schedule.sponsor.clone(),
            amount: schedule.amount,
            start,
            cancelled: false,
        };
        env.storage().persistent().set(&stream_key, &stream);

        schedule.spawned += 1;
        env.storage().persistent().set(&key, &schedule);

        Ok(stream_id)
    }

    /// Cancel a single stream instance without affecting its siblings.
    /// Only the sender (`owner`) may cancel; a sponsor has no such privilege.
    pub fn cancel_stream(env: Env, stream_id: u64) -> Result<(), Error> {
        let key = DataKey {
            kind: KEY_STREAM,
            id: stream_id,
        };
        let mut stream: Stream = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::StreamNotFound)?;
        stream.owner.require_auth();
        stream.cancelled = true;
        env.storage().persistent().set(&key, &stream);
        Ok(())
    }

    /// Set the protocol rate and duration ceilings. `0` disables a ceiling.
    /// Applies only to streams/schedules created after this call; existing
    /// streams retain their agreed terms.
    pub fn set_policy(env: Env, admin: Address, policy: Policy) -> Result<(), Error> {
        admin.require_auth();
        if policy.max_rate < 0 {
            return Err(Error::InvalidAmount);
        }
        env.storage()
            .persistent()
            .set(&DataKey { kind: KEY_POLICY, id: 0 }, &policy);
        Ok(())
    }

    pub

/* … truncated 5485 chars — edit only what you need near the top … */
