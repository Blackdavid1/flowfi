//! Recurring stream contract.
//!
//! Streams can be created directly (single instance) or from a bounded
//! recurring schedule. Each instance produced by a schedule is independently
//! funded, uniquely identified, and independently cancellable. Linkage between
//! instances is deterministic: `(schedule_id, index)`.
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
    pub fn create_stream(
        env: Env,
        owner: Address,
        recipient: Address,
        amount: i128,
        start: u64,
    ) -> Result<u64, Error> {
        owner.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let id = next_id(&env, KEY_NEXT_STREAM);
        let stream = Stream {
            id,
            owner,
            recipient,
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
    pub fn create_schedule(
        env: Env,
        owner: Address,
        recipient: Address,
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
        enforce_ceilings(&env, amount, interval, count)?;
        let id = next_id(&env, KEY_NEXT_SCHEDULE);
        let schedule = Schedule {
            id,
            owner,
            recipient,
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
    /// covering `amount`) and independently cancellable.
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

    pub fn get_policy(env: Env) -> Policy {
        env.storage()
            .persistent()
            .get(&DataKey { kind: KEY_POLICY, id: 0 })
            .unwrap_or(Policy {
                max_rate: 0,
                max_duration: 0,
            })
    }

    pub fn get_stream(env: Env, stream_id: u64) -> Option<Stream> {
        env.storage()
            .persistent()
            .get(&DataKey {
                kind: KEY_STREAM,
                id: stream_id,
            })
    }

    pub fn get_schedule(env: Env, schedule_id: u64) -> Option<Schedule> {
        env.storage()
            .persistent()
            .get(&DataKey {
                kind: KEY_SCHEDULE,
                id: schedule_id,
            })
    }
}

/// Enforce the current policy ceilings against a proposed schedule.
/// A ceiling of `0` is treated as unbounded.
fn enforce_ceilings(env: &Env, amount: i128, interval: u64, count: u32) -> Result<(), Error> {
    let policy: Policy = env
        .storage()
        .persistent()
        .get(&DataKey { kind: KEY_POLICY, id: 0 })
        .unwrap_or(Policy {
            max_rate: 0,
            max_duration: 0,
        });

    if policy.max_rate > 0 {
        // Rate in base units per second, integer division.
        let rate = amount / (interval as i128);
        if rate > policy.max_rate {
            return Err(Error::RateExceedsCeiling);
        }
    }

    if policy.max_duration > 0 {
        let duration = interval
            .checked_mul(count as u64)
            .ok_or(Error::DurationExceedsCeiling)?;
        if duration > policy.max_duration {
            return Err(Error::DurationExceedsCeiling);
        }
    }

    Ok(())
}

fn next_id(env: &Env, kind: u32) -> u64 {
    let key = DataKey { kind, id: 0 };
    let current: u64 = env.storage().persistent().get(&key).unwrap_or(0);
    let next = current + 1;
    env.storage().persistent().set(&key, &next);
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, StreamContractClient<'static>, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, StreamContract);
        let client = StreamContractClient::new(&env, &contract_id);
        let owner = Address::generate(&env);
        let recipient = Address::generate(&env);
        (env, client, owner, recipient)
    }

    #[test]
    fn rejects_out_of_bounds_schedule() {
        let (_env, client, owner, recipient) = setup();
        assert_eq!(
            client.try_create_schedule(&owner, &recipient, &100, &0, &1, &3),
            Err(Ok(Error::InvalidInterval))
        );
        assert_eq!(
            client.try_create_schedule(&owner, &recipient, &100, &0, &MIN_INTERVAL, &0),
            Err(Ok(Error::InvalidCount))
        );
    }

    #[test]
    fn rate_ceiling_boundaries() {
        let (_env, client, owner, recipient) = setup();
        let admin = Address::generate(&_env);
        // max_rate = 10 base units / second.
        client.set_policy(&admin, &Policy { max_rate: 10, max_duration: 0 });

        // interval = 60s. amount = 600 -> rate 10 (exactly at ceiling).
        assert!(client
            .try_create_schedule(&owner, &recipient, &600, &0, &MIN_INTERVAL, &1)
            .is_ok());
        // amount = 601 -> rate 10 (integer division) still at ceiling.
        assert!(client
            .try_create_schedule(&owner, &recipient, &601, &0, &MIN_INTERVAL, &1)
            .is_ok());
        // amount = 660 -> rate 11 (just above ceiling).
        assert_eq!(
            client.try_create_schedule(&owner, &recipient, &660, &0, &MIN_INTERVAL, &1),
            Err(Ok(Error::RateExceedsCeiling))
        );
    }

    #[test]
    fn duration_ceiling_boundaries() {
        let (_env, client, owner, recipient) = setup();
        let admin = Address::generate(&_env);
        // max_duration = 600s.
        client.set_policy(&admin, &Policy { max_rate: 0, max_duration: 600 });

        // interval 60 * count 10 = 600 (exactly at ceiling).
        assert!(client
            .try_create_schedule(&owner, &recipient, &100, &0, &MIN_INTERVAL, &10)
            .is_ok());
        // interval 60 * count 11 = 660 (just above ceiling).
        assert_eq!(
            client.try_create_schedule(&owner, &recipient, &100, &0, &MIN_INTERVAL, &11),
            Err(Ok(Error::DurationExceedsCeiling))
        );
    }

    #[test]
    fn existing_streams_unaffected_by_policy_change() {
        let (_env, client, owner, recipient) = setup();
        let admin = Address::generate(&_env);

        // Create a schedule with no ceilings in place.
        let id = client
            .create_schedule(&owner, &recipient, &100_000, &0, &MIN_INTERVAL, &10)
            .unwrap();

        // Tighten policy afterwards.
        client.set_policy(&admin, &Policy { max_rate: 1, max_duration: 60 });

        // Existing schedule retains its agreed terms and can still spawn.
        let schedule = client.get_schedule(&id).unwrap();
        assert_eq!(schedule.amount, 100_000);
        assert_eq!(schedule.count, 10);
        assert!(client.spawn_from_schedule(&id, &100_000).is_ok());

        // New schedules exceeding the tightened ceilings are rejected.
        assert_eq!(
            client.try_create_schedule(&owner, &recipient, &100_000, &0, &MIN_INTERVAL, &10),
            Err(Ok(Error::RateExceedsCeiling))
        );
    }
}
