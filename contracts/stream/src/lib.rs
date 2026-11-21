//! Recurring stream contract.
//!
//! Streams can be created directly (single instance) or from a bounded
//! recurring schedule. Each instance produced by a schedule is independently
//! funded, uniquely identified, and independently cancellable. Linkage between
//! instances is deterministic: `(schedule_id, index)`.

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
        assert_eq!(
            client.try_create_schedule(
                &owner,
                &recipient,
                &100,
                &0,
                &MIN_INTERVAL,
                &(MAX_INSTANCES + 1)
            ),
            Err(Ok(Error::InvalidCount))
        );
    }

    #[test]
    fn spawns_deterministic_linked_instances() {
        let (_env, client, owner, recipient) = setup();
        let sid = client.create_schedule(&owner, &recipient, &100, &1000, &MIN_INTERVAL, &3);
        let s0 = client.spawn_from_schedule(&sid, &100);
        let s1 = client.spawn_from_schedule(&sid, &100);
        assert_eq!(s0, sid * 1_000_000);
        assert_eq!(s1, sid * 1_000_000 + 1);
        let stream0 = client.get_stream(&s0).unwrap();
        let stream1 = client.get_stream(&s1).unwrap();
        assert_eq!(stream0.start, 1000);
        assert_eq!(stream1.start, 1000 + MIN_INTERVAL);
    }

    #[test]
    fn rejects_insufficient_funding_and_exhaustion() {
        let (_env, client, owner, recipient) = setup();
        let sid = client.create_schedule(&owner, &recipient, &100, &0, &MIN_INTERVAL, &1);
        assert_eq!(
            client.try_spawn_from_schedule(&sid, &99),
            Err(Ok(Error::InsufficientFunding))
        );
        client.spawn_from_schedule(&sid, &100);
        assert_eq!(
            client.try_spawn_from_schedule(&sid, &100),
            Err(Ok(Error::ScheduleExhausted))
        );
    }

    #[test]
    fn instances_cancel_independently() {
        let (_env, client, owner, recipient) = setup();
        let sid = client.create_schedule(&owner, &recipient, &100, &0, &MIN_INTERVAL, &2);
        let s0 = client.spawn_from_schedule(&sid, &100);
        let s1 = client.spawn_from_schedule(&sid, &100);
        client.cancel_stream(&s0);
        assert!(client.get_stream(&s0).unwrap().cancelled);
        assert!(!client.get_stream(&s1).unwrap().cancelled);
    }
}
