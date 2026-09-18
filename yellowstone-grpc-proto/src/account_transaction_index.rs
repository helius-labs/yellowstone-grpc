/// Getter-only view of an account write's origin. Indices are zero-based;
/// legacy omitted metadata is indistinguishable from Transaction(0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountTransactionIndex {
    Transaction(u64),
    NoTransaction,
}

impl AccountTransactionIndex {
    pub const fn from_wire(value: u64) -> Self {
        match value {
            u64::MAX => Self::NoTransaction,
            index => Self::Transaction(index),
        }
    }
}

impl crate::geyser::SubscribeUpdateAccountInfo {
    pub const fn account_transaction_index(&self) -> AccountTransactionIndex {
        AccountTransactionIndex::from_wire(self.transaction_index)
    }
}

#[cfg(test)]
mod tests {
    use super::AccountTransactionIndex;
    use crate::geyser::SubscribeUpdateAccountInfo;

    #[test]
    fn account_transaction_index() {
        for value in [0, 42, u64::MAX] {
            let account = SubscribeUpdateAccountInfo {
                transaction_index: value,
                ..Default::default()
            };
            let expected = if value == u64::MAX {
                AccountTransactionIndex::NoTransaction
            } else {
                AccountTransactionIndex::Transaction(value)
            };
            assert_eq!(account.account_transaction_index(), expected);
        }
    }
}
