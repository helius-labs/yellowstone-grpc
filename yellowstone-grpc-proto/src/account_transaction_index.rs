/// Origin of an account write. Transaction indices are zero-based and unshifted.
/// Old producers that omit field 32 are indistinguishable from `Transaction(0)`.
/// Raw enum construction can bypass validation. Use the checked
/// `SubscribeUpdateAccountInfo::set_account_transaction_index` setter to reject
/// `Transaction(u64::MAX)`; `to_wire` only maps the stored value.
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

    /// Map the stored value to the raw scalar without validating it.
    /// Even a manually constructed `Transaction(u64::MAX)` maps to `u64::MAX`,
    /// which decodes as `NoTransaction`.
    pub const fn to_wire(self) -> u64 {
        match self {
            Self::Transaction(index) => index,
            Self::NoTransaction => u64::MAX,
        }
    }
}

impl crate::geyser::SubscribeUpdateAccountInfo {
    /// Typed view of the raw protobuf scalar (also available on block accounts).
    pub const fn account_transaction_index(&self) -> AccountTransactionIndex {
        AccountTransactionIndex::from_wire(self.transaction_index)
    }

    /// Set the raw scalar without shifting; reject the reserved transaction index.
    /// On error, the account is unchanged.
    pub fn set_account_transaction_index(
        &mut self,
        index: AccountTransactionIndex,
    ) -> Result<(), ReservedTransactionIndex> {
        if index == AccountTransactionIndex::Transaction(u64::MAX) {
            return Err(ReservedTransactionIndex);
        }
        self.transaction_index = index.to_wire();
        Ok(())
    }
}
