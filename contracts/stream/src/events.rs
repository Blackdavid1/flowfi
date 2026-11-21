use soroban_sdk::{contracttype, Address, Env};

/// Emitted when a new stream is created.
///
/// `sponsor` is `Some(addr)` when a third party covered the transaction costs
/// and/or initial funding for the stream. The sponsor never receives sender or
/// recipient privileges; those remain bound to `sender` and `recipient`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCreated {
    pub stream_id: u64,
    pub sender: Address,
    pub recipient: Address,
    pub sponsor: Option<Address>,
    pub amount: i128,
}

/// Emitted when a sponsor funds or covers costs for an existing stream.
///
/// Kept separate from `StreamCreated` so sponsorship attribution is explicit
/// and never conflated with sender/recipient roles.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamSponsored {
    pub stream_id: u64,
    pub sponsor: Address,
    pub amount: i128,
}

pub fn emit_stream_created(
    env: &Env,
    stream_id: u64,
    sender: &Address,
    recipient: &Address,
    sponsor: &Option<Address>,
    amount: i128,
) {
    let event = StreamCreated {
        stream_id,
        sender: sender.clone(),
        recipient: recipient.clone(),
        sponsor: sponsor.clone(),
        amount,
    };
    env.events().publish(("stream_created",), event);
}

pub fn emit_stream_sponsored(env: &Env, stream_id: u64, sponsor: &Address, amount: i128) {
    let event = StreamSponsored {
        stream_id,
        sponsor: sponsor.clone(),
        amount,
    };
    env.events().publish(("stream_sponsored",), event);
}
