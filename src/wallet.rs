use zeroize::Zeroizing;

use monero_wallet::{
    address::{AddressType, MoneroAddress, Network, SubaddressIndex},
    ed25519::Scalar,
    ViewPair,
};

/// Errors raised when loading a merchant's wallet credentials.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum WalletError {
    #[error("invalid address")]
    InvalidAddress,
    #[error("a primary address is required, not a subaddress or an integrated address")]
    NotPrimaryAddress,
    #[error("the private view key must be 64 hexadecimal characters encoding a valid scalar")]
    InvalidViewKey,
    #[error("the private view key does not belong to this address")]
    ViewKeyMismatch,
    #[error("subaddress index out of range")]
    InvalidSubaddressIndex,
}

/// A view-only wallet: a primary address and its private view key.
///
/// It can derive subaddresses and detect incoming payments. It cannot spend.
#[derive(Clone)]
pub struct ViewWallet {
    pair: ViewPair,
    network: Network,
}

impl ViewWallet {
    /// Load a wallet from a primary address and a hex-encoded private view key.
    ///
    /// The network is read from the address. The view key is checked against the address, so a
    /// typo in either is caught here instead of silently missing payments.
    pub fn new(primary_address: &str, private_view_key: &str) -> Result<Self, WalletError> {
        let address = MoneroAddress::from_str_with_unchecked_network(primary_address.trim())
            .map_err(|_| WalletError::InvalidAddress)?;
        if *address.kind() != AddressType::Legacy {
            return Err(WalletError::NotPrimaryAddress);
        }

        let mut key = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(private_view_key.trim(), key.as_mut_slice())
            .map_err(|_| WalletError::InvalidViewKey)?;
        let view = Zeroizing::new(
            Scalar::read(&mut key.as_slice()).map_err(|_| WalletError::InvalidViewKey)?,
        );

        let pair = ViewPair::new(address.spend(), view).map_err(|_| WalletError::InvalidAddress)?;
        if pair.view() != address.view() {
            return Err(WalletError::ViewKeyMismatch);
        }

        Ok(Self {
            pair,
            network: address.network(),
        })
    }

    /// The network this wallet belongs to.
    pub fn network(&self) -> Network {
        self.network
    }

    /// The primary address.
    pub fn primary_address(&self) -> String {
        self.pair.legacy_address(self.network).to_string()
    }

    /// The subaddress at `(account, index)`. `(0, 0)` is the primary address and is rejected.
    pub fn subaddress(&self, account: u32, index: u32) -> Result<String, WalletError> {
        let index =
            SubaddressIndex::new(account, index).ok_or(WalletError::InvalidSubaddressIndex)?;
        Ok(self.pair.subaddress(self.network, index).to_string())
    }

    pub(crate) fn pair(&self) -> &ViewPair {
        &self.pair
    }
}
