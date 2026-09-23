/// Getter-only view of an account write's origin. Indices are zero-based;
/// legacy omitted metadata is indistinguishable from Transaction(0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountIndex {
    NoTransaction,
    Transaction(u64),
}

impl AccountIndex {
    pub const fn from_wire(transaction_index: u64) -> Self {
        match transaction_index {
            u64::MAX => Self::NoTransaction,
            index => Self::Transaction(index),
        }
    }
}

impl crate::geyser::SubscribeUpdateAccountInfo {
    pub const fn account_index(&self) -> AccountIndex {
        AccountIndex::from_wire(self.transaction_index)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::AccountIndex,
        crate::{geyser::SubscribeUpdateAccountInfo, prost::Message},
    };

    #[test]
    fn account_index() {
        assert_eq!(decode(&[]), AccountIndex::Transaction(0));
        assert_eq!(decode(&[0x80, 0x02, 0]), AccountIndex::Transaction(0));
        assert_eq!(decode(&[0x80, 0x02, 42]), AccountIndex::Transaction(42));
        assert_eq!(
            decode(&[0x80, 0x02, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1]),
            AccountIndex::NoTransaction
        );
    }

    fn decode(wire: &[u8]) -> AccountIndex {
        SubscribeUpdateAccountInfo::decode(wire)
            .unwrap()
            .account_index()
    }
}
