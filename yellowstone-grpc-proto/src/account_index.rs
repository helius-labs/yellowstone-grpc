/// Getter-only view of an account write's origin. Indices are zero-based;
/// legacy omitted metadata is indistinguishable from TransactionIndex(0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountIndex {
    /// Zero-based native operation count for this pubkey within its bank.
    NativeOperation(u64),
    TransactionIndex(u64),
}

impl AccountIndex {
    pub const fn from_wire(transaction_index: u64, native_operation_count: u64) -> Self {
        match transaction_index {
            u64::MAX => Self::NativeOperation(native_operation_count),
            index => Self::TransactionIndex(index),
        }
    }
}

impl crate::geyser::SubscribeUpdateAccountInfo {
    pub const fn account_index(&self) -> AccountIndex {
        AccountIndex::from_wire(self.transaction_index, self.native_operation_count)
    }
}

#[cfg(test)]
mod tests {
    use super::AccountIndex;
    use crate::{geyser::SubscribeUpdateAccountInfo, prost::Message};

    #[test]
    fn account_index() {
        for (wire, expected) in [
            (&[][..], AccountIndex::TransactionIndex(0)),
            (&[0x80, 0x02, 0][..], AccountIndex::TransactionIndex(0)),
            (
                &[0x80, 0x02, 42, 0x88, 0x02, 9][..],
                AccountIndex::TransactionIndex(42),
            ),
            (
                &[0x80, 0x02, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1][..],
                AccountIndex::NativeOperation(0),
            ),
            (
                &[
                    0x80, 0x02, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1, 0x88, 0x02, 7,
                ][..],
                AccountIndex::NativeOperation(7),
            ),
        ] {
            let account = SubscribeUpdateAccountInfo::decode(wire).unwrap();
            assert_eq!(account.account_index(), expected);
        }
    }
}
