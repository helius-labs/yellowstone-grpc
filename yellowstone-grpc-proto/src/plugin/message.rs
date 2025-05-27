use {
    crate::{
        convert_to,
        geyser::{
            subscribe_update::UpdateOneof, CommitmentLevel as CommitmentLevelProto,
            SlotStatus as SlotStatusProto, SubscribeUpdateAccount, SubscribeUpdateAccountInfo,
            SubscribeUpdateBlock, SubscribeUpdateBlockMeta, SubscribeUpdateEntry,
            SubscribeUpdateSlot, SubscribeUpdateTransaction, SubscribeUpdateTransactionInfo,
        },
        solana::storage::confirmed_block,
    },
    agave_geyser_plugin_interface::geyser_plugin_interface::{
        ReplicaAccountInfoV3, ReplicaBlockInfoV4, ReplicaEntryInfoV2, ReplicaTransactionInfoV2,
        SlotStatus as GeyserSlotStatus,
    },
    prost_types::Timestamp,
    solana_sdk::{
        clock::Slot,
        hash::{Hash, HASH_BYTES},
        pubkey::Pubkey,
        signature::Signature,
    },
    std::{
        collections::HashSet,
        ops::{Deref, DerefMut},
        sync::Arc,
        time::SystemTime,
    },
};

type FromUpdateOneofResult<T> = Result<T, &'static str>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommitmentLevel {
    Processed,
    Confirmed,
    Finalized,
}

impl From<CommitmentLevel> for CommitmentLevelProto {
    fn from(commitment: CommitmentLevel) -> Self {
        match commitment {
            CommitmentLevel::Processed => Self::Processed,
            CommitmentLevel::Confirmed => Self::Confirmed,
            CommitmentLevel::Finalized => Self::Finalized,
        }
    }
}

impl From<CommitmentLevelProto> for CommitmentLevel {
    fn from(status: CommitmentLevelProto) -> Self {
        match status {
            CommitmentLevelProto::Processed => Self::Processed,
            CommitmentLevelProto::Confirmed => Self::Confirmed,
            CommitmentLevelProto::Finalized => Self::Finalized,
        }
    }
}

impl CommitmentLevel {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Processed => "processed",
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotStatus {
    Processed,
    Confirmed,
    Finalized,
    FirstShredReceived,
    Completed,
    CreatedBank,
    Dead,
}

impl From<&GeyserSlotStatus> for SlotStatus {
    fn from(status: &GeyserSlotStatus) -> Self {
        match status {
            GeyserSlotStatus::Processed => Self::Processed,
            GeyserSlotStatus::Confirmed => Self::Confirmed,
            GeyserSlotStatus::Rooted => Self::Finalized,
            GeyserSlotStatus::FirstShredReceived => Self::FirstShredReceived,
            GeyserSlotStatus::Completed => Self::Completed,
            GeyserSlotStatus::CreatedBank => Self::CreatedBank,
            GeyserSlotStatus::Dead(_error) => Self::Dead,
        }
    }
}

impl From<SlotStatusProto> for SlotStatus {
    fn from(status: SlotStatusProto) -> Self {
        match status {
            SlotStatusProto::SlotProcessed => Self::Processed,
            SlotStatusProto::SlotConfirmed => Self::Confirmed,
            SlotStatusProto::SlotFinalized => Self::Finalized,
            SlotStatusProto::SlotFirstShredReceived => Self::FirstShredReceived,
            SlotStatusProto::SlotCompleted => Self::Completed,
            SlotStatusProto::SlotCreatedBank => Self::CreatedBank,
            SlotStatusProto::SlotDead => Self::Dead,
        }
    }
}

impl From<SlotStatus> for SlotStatusProto {
    fn from(status: SlotStatus) -> Self {
        match status {
            SlotStatus::Processed => Self::SlotProcessed,
            SlotStatus::Confirmed => Self::SlotConfirmed,
            SlotStatus::Finalized => Self::SlotFinalized,
            SlotStatus::FirstShredReceived => Self::SlotFirstShredReceived,
            SlotStatus::Completed => Self::SlotCompleted,
            SlotStatus::CreatedBank => Self::SlotCreatedBank,
            SlotStatus::Dead => Self::SlotDead,
        }
    }
}

impl PartialEq<SlotStatus> for CommitmentLevel {
    fn eq(&self, other: &SlotStatus) -> bool {
        match self {
            Self::Processed if *other == SlotStatus::Processed => true,
            Self::Confirmed if *other == SlotStatus::Confirmed => true,
            Self::Finalized if *other == SlotStatus::Finalized => true,
            _ => false,
        }
    }
}

impl SlotStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Processed => "processed",
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
            Self::FirstShredReceived => "first_shread_received",
            Self::Completed => "completed",
            Self::CreatedBank => "created_bank",
            Self::Dead => "dead",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageSlot {
    pub slot: Slot,
    pub parent: Option<Slot>,
    pub status: SlotStatus,
    pub dead_error: Option<String>,
    pub created_at: Timestamp,
    pub sequence_number: u64,
}

impl MessageSlot {
    pub fn from_geyser(slot: Slot, parent: Option<Slot>, status: &GeyserSlotStatus, sequence_number: u64) -> Self {
        Self {
            slot,
            parent,
            status: status.into(),
            dead_error: if let GeyserSlotStatus::Dead(error) = status {
                Some(error.clone())
            } else {
                None
            },
            created_at: Timestamp::from(SystemTime::now()),
            sequence_number,
        }
    }

}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageAccountInfo {
    pub pubkey: Pubkey,
    pub lamports: u64,
    pub owner: Pubkey,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
    pub write_version: u64,
    pub txn_signature: Option<Signature>,
}

impl MessageAccountInfo {
    pub fn from_geyser(info: &ReplicaAccountInfoV3<'_>) -> Self {
        Self {
            pubkey: Pubkey::try_from(info.pubkey).expect("valid Pubkey"),
            lamports: info.lamports,
            owner: Pubkey::try_from(info.owner).expect("valid Pubkey"),
            executable: info.executable,
            rent_epoch: info.rent_epoch,
            data: info.data.into(),
            write_version: info.write_version,
            txn_signature: info.txn.map(|txn| *txn.signature()),
        }
    }

    pub fn from_update_oneof(msg: SubscribeUpdateAccountInfo) -> FromUpdateOneofResult<Self> {
        Ok(Self {
            pubkey: Pubkey::try_from(msg.pubkey.as_slice()).map_err(|_| "invalid pubkey length")?,
            lamports: msg.lamports,
            owner: Pubkey::try_from(msg.owner.as_slice()).map_err(|_| "invalid owner length")?,
            executable: msg.executable,
            rent_epoch: msg.rent_epoch,
            data: msg.data,
            write_version: msg.write_version,
            txn_signature: msg
                .txn_signature
                .map(|sig| {
                    Signature::try_from(sig.as_slice()).map_err(|_| "invalid signature length")
                })
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageAccount {
    pub account: Arc<MessageAccountInfo>,
    pub slot: Slot,
    pub is_startup: bool,
    pub created_at: Timestamp,
    pub sequence_number: u64,
}

impl MessageAccount {
    pub fn from_geyser(info: &ReplicaAccountInfoV3<'_>, slot: Slot, is_startup: bool, sequence_number: u64) -> Self {
        Self {
            account: Arc::new(MessageAccountInfo::from_geyser(info)),
            slot,
            is_startup,
            created_at: Timestamp::from(SystemTime::now()),
            sequence_number,
        }
    }

}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageTransactionInfo {
    pub signature: Signature,
    pub is_vote: bool,
    pub transaction: confirmed_block::Transaction,
    pub meta: confirmed_block::TransactionStatusMeta,
    pub index: usize,
    pub account_keys: HashSet<Pubkey>,
    pub sequence_number: u64,
}

impl MessageTransactionInfo {
    pub fn from_geyser(info: &ReplicaTransactionInfoV2<'_>, sequence_number: u64) -> Self {
        let account_keys = info
            .transaction
            .message()
            .account_keys()
            .iter()
            .copied()
            .collect();

        Self {
            signature: *info.signature,
            is_vote: info.is_vote,
            transaction: convert_to::create_transaction(info.transaction),
            meta: convert_to::create_transaction_meta(info.transaction_status_meta),
            index: info.index,
            account_keys,
            sequence_number,
        }
    }



    pub fn fill_account_keys(&mut self) -> FromUpdateOneofResult<()> {
        let mut account_keys = HashSet::new();

        // static
        if let Some(pubkeys) = self
            .transaction
            .message
            .as_ref()
            .map(|msg| msg.account_keys.as_slice())
        {
            for pubkey in pubkeys {
                account_keys.insert(
                    Pubkey::try_from(pubkey.as_slice()).map_err(|_| "invalid pubkey length")?,
                );
            }
        }

        // dynamic
        for pubkey in self.meta.loaded_writable_addresses.iter() {
            account_keys
                .insert(Pubkey::try_from(pubkey.as_slice()).map_err(|_| "invalid pubkey length")?);
        }
        for pubkey in self.meta.loaded_readonly_addresses.iter() {
            account_keys
                .insert(Pubkey::try_from(pubkey.as_slice()).map_err(|_| "invalid pubkey length")?);
        }

        self.account_keys = account_keys;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageTransaction {
    pub transaction: Arc<MessageTransactionInfo>,
    pub slot: u64,
    pub created_at: Timestamp,
}

impl MessageTransaction {
    pub fn from_geyser(info: &ReplicaTransactionInfoV2<'_>, slot: Slot, sequence_number: u64) -> Self {
        Self {
            transaction: Arc::new(MessageTransactionInfo::from_geyser(info, sequence_number)),
            slot,
            created_at: Timestamp::from(SystemTime::now()),
        }
    }

}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MessageEntry {
    pub slot: u64,
    pub index: usize,
    pub num_hashes: u64,
    pub hash: Hash,
    pub executed_transaction_count: u64,
    pub starting_transaction_index: u64,
    pub created_at: Timestamp,
    pub sequence_number: u64,
}

impl MessageEntry {
    pub fn from_geyser(info: &ReplicaEntryInfoV2, sequence_number: u64) -> Self {
        Self {
            slot: info.slot,
            index: info.index,
            num_hashes: info.num_hashes,
            hash: Hash::new_from_array(<[u8; HASH_BYTES]>::try_from(info.hash).unwrap()),
            executed_transaction_count: info.executed_transaction_count,
            starting_transaction_index: info
                .starting_transaction_index
                .try_into()
                .expect("failed convert usize to u64"),
            created_at: Timestamp::from(SystemTime::now()),
            sequence_number,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageBlockMeta {
    pub block_meta: SubscribeUpdateBlockMeta,
    pub created_at: Timestamp,
    pub sequence_number: u64,
}

impl Deref for MessageBlockMeta {
    type Target = SubscribeUpdateBlockMeta;

    fn deref(&self) -> &Self::Target {
        &self.block_meta
    }
}

impl DerefMut for MessageBlockMeta {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.block_meta
    }
}

impl MessageBlockMeta {
    pub fn from_geyser(info: &ReplicaBlockInfoV4<'_>, sequence_number: u64) -> Self {
        Self {
            block_meta: SubscribeUpdateBlockMeta {
                parent_slot: info.parent_slot,
                slot: info.slot,
                parent_blockhash: info.parent_blockhash.to_string(),
                blockhash: info.blockhash.to_string(),
                rewards: Some(convert_to::create_rewards_obj(
                    &info.rewards.rewards,
                    info.rewards.num_partitions,
                )),
                block_time: info.block_time.map(convert_to::create_timestamp),
                block_height: info.block_height.map(convert_to::create_block_height),
                executed_transaction_count: info.executed_transaction_count,
                entries_count: info.entry_count,
            },
            created_at: Timestamp::from(SystemTime::now()),
            sequence_number,
        }
    }

}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageBlock {
    pub meta: Arc<MessageBlockMeta>,
    pub transactions: Vec<Arc<MessageTransactionInfo>>,
    pub updated_account_count: u64,
    pub accounts: Vec<Arc<MessageAccountInfo>>,
    pub entries: Vec<Arc<MessageEntry>>,
    pub created_at: Timestamp,
}

impl MessageBlock {
    pub fn new(
        meta: Arc<MessageBlockMeta>,
        transactions: Vec<Arc<MessageTransactionInfo>>,
        accounts: Vec<Arc<MessageAccountInfo>>,
        entries: Vec<Arc<MessageEntry>>,
    ) -> Self {
        Self {
            meta,
            transactions,
            updated_account_count: accounts.len() as u64,
            accounts,
            entries,
            created_at: Timestamp::from(SystemTime::now()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Slot(MessageSlot),
    Account(MessageAccount),
    Transaction(MessageTransaction),
    Entry(Arc<MessageEntry>),
    BlockMeta(Arc<MessageBlockMeta>),
    Block(Arc<MessageBlock>),
}

impl Message {

    pub fn get_sequence_number(&self) -> u64 {
        match self {
            Self::Slot(msg) => msg.sequence_number,
            Self::Account(msg) => msg.sequence_number,
            Self::Transaction(msg) => msg.transaction.sequence_number,
            Self::Entry(msg) => msg.sequence_number,
            Self::BlockMeta(msg) => msg.sequence_number,
            Self::Block(msg) => msg.meta.sequence_number,
        }
    }

    pub fn get_slot(&self) -> u64 {
        match self {
            Self::Slot(msg) => msg.slot,
            Self::Account(msg) => msg.slot,
            Self::Transaction(msg) => msg.slot,
            Self::Entry(msg) => msg.slot,
            Self::BlockMeta(msg) => msg.slot,
            Self::Block(msg) => msg.meta.slot,
        }
    }

    pub fn created_at(&self) -> Timestamp {
        match self {
            Self::Slot(msg) => msg.created_at,
            Self::Account(msg) => msg.created_at,
            Self::Transaction(msg) => msg.created_at,
            Self::Entry(msg) => msg.created_at,
            Self::BlockMeta(msg) => msg.created_at,
            Self::Block(msg) => msg.created_at,
        }
    }

    pub fn time_since_created_ms(&self) -> f64 {
        time_since_timestamp(self.created_at())
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Slot(_) => "slot",
            Self::Account(_) => "account",
            Self::Transaction(_) => "transaction",
            Self::Entry(_) => "entry",
            Self::BlockMeta(_) => "block_meta",
            Self::Block(_) => "block",
        }
    }
}


pub fn time_since_timestamp(timestamp: Timestamp) -> f64 {
    let current_time = Timestamp::from(SystemTime::now());
    ((current_time.seconds - timestamp.seconds) as f64 * 1000.0) + ((current_time.nanos - timestamp.nanos) as f64 / 1_000_000.0)
}