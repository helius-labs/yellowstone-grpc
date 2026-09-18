/// Origin of an account write. Transaction indices are zero-based and unshifted.
/// Old producers that omit field 32 are indistinguishable from `Transaction(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountTransactionIndex {
    Transaction(u64),
    NoTransaction,
}

/// `Transaction(u64::MAX)` is reserved; use `NoTransaction` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedTransactionIndex;

impl std::fmt::Display for ReservedTransactionIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("transaction index is reserved for NoTransaction")
    }
}
impl std::error::Error for ReservedTransactionIndex {}

impl AccountTransactionIndex {
    pub const fn from_wire(value: u64) -> Self {
        match value {
            u64::MAX => Self::NoTransaction,
            index => Self::Transaction(index),
        }
    }

    pub const fn to_wire(self) -> Result<u64, ReservedTransactionIndex> {
        match self {
            Self::Transaction(u64::MAX) => Err(ReservedTransactionIndex),
            Self::Transaction(index) => Ok(index),
            Self::NoTransaction => Ok(u64::MAX),
        }
    }
}

impl crate::geyser::SubscribeUpdateAccountInfo {
    /// Typed view of the raw protobuf scalar (also available on block accounts).
    pub const fn account_transaction_index(&self) -> AccountTransactionIndex {
        AccountTransactionIndex::from_wire(self.transaction_index)
    }

    /// Set the raw scalar without shifting; reject the reserved transaction index.
    pub fn set_account_transaction_index(
        &mut self,
        index: AccountTransactionIndex,
    ) -> Result<(), ReservedTransactionIndex> {
        self.transaction_index = index.to_wire()?;
        Ok(())
    }
}
