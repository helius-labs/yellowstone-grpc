/// Read-only typed view of an account write's origin.
/// Transaction indices are zero-based and unshifted. Old producers that omit
/// field 32 are indistinguishable from `Transaction(0)`.
/// The decoder maps `u64::MAX` to `NoTransaction`, never `Transaction(u64::MAX)`.
/// Generated protobuf fields remain available for wire compatibility, but this
/// typed API deliberately provides no setter or encoder.
///
/// ```compile_fail,E0599
/// use laserstream_core_proto::geyser::{AccountTransactionIndex, SubscribeUpdateAccountInfo};
/// let mut account = SubscribeUpdateAccountInfo::default();
/// account.set_account_transaction_index(AccountTransactionIndex::Transaction(42));
/// ```
///
/// ```compile_fail,E0599
/// use laserstream_core_proto::AccountTransactionIndex;
/// AccountTransactionIndex::NoTransaction.to_wire();
/// ```
///
/// ```compile_fail,E0432
/// use laserstream_core_proto::ReservedTransactionIndex;
/// ```
///
/// ```compile_fail,E0432
/// use laserstream_core_proto::geyser::ReservedTransactionIndex;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountTransactionIndex {
    Transaction(u64),
    NoTransaction,
}

impl AccountTransactionIndex {
    /// Decode the raw scalar, including legacy omission (zero).
    pub const fn from_wire(value: u64) -> Self {
        match value {
            u64::MAX => Self::NoTransaction,
            index => Self::Transaction(index),
        }
    }
}

impl crate::geyser::SubscribeUpdateAccountInfo {
    /// Typed view of the raw protobuf scalar (also available on block accounts).
    pub const fn account_transaction_index(&self) -> AccountTransactionIndex {
        AccountTransactionIndex::from_wire(self.transaction_index)
    }
}
