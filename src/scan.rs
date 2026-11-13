use monero_wallet::{
    address::SubaddressIndex,
    block::{Block, BlockHeader},
    interface::ScannableBlock,
    transaction::{Input, Pruned, Timelock, Transaction, TransactionPrefix},
    Scanner,
};

use crate::wallet::{ViewWallet, WalletError};

/// The last protocol version the scanner understands.
const SUPPORTED_HARDFORK: u8 = 16;

/// Errors raised while scanning.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ScanError {
    /// The chain moved to a protocol version this build cannot scan. Payments would be missed, so
    /// scanning stops instead of continuing blind.
    #[error("unsupported protocol version {0}: upgrade required")]
    UnsupportedProtocol(u8),
    #[error("malformed block or transaction data: {0}")]
    Malformed(&'static str),
}

impl From<monero_wallet::ScanError> for ScanError {
    fn from(err: monero_wallet::ScanError) -> Self {
        match err {
            monero_wallet::ScanError::UnsupportedProtocol(v) => ScanError::UnsupportedProtocol(v),
            monero_wallet::ScanError::InvalidScannableBlock(why) => ScanError::Malformed(why),
        }
    }
}

/// An output received by the wallet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Payment {
    /// Hash of the transaction carrying the output.
    pub txid: [u8; 32],
    /// Position of the output within the transaction.
    pub output_index: u64,
    /// `(account, index)` of the receiving subaddress, or `None` for the primary address.
    pub subaddress: Option<(u32, u32)>,
    /// Amount in piconero.
    pub amount: u64,
    /// The one-time output key.
    ///
    /// Callers MUST reject a payment whose key was already seen in an earlier payment: only one
    /// of the outputs sharing a key can ever be spent (the "burning bug").
    pub output_key: [u8; 32],
}

/// Scans blocks and mempool transactions for payments to a [`ViewWallet`].
///
/// Outputs carrying an additional timelock are ignored: a payer could otherwise pay with funds
/// the merchant cannot spend for an arbitrary time.
#[derive(Clone)]
pub struct PaymentScanner {
    scanner: Scanner,
}

impl PaymentScanner {
    /// Create a scanner watching the primary address only.
    pub fn new(wallet: &ViewWallet) -> Self {
        Self {
            scanner: Scanner::new(wallet.pair().clone()),
        }
    }

    /// Start watching a subaddress. Payments to unwatched subaddresses are not detected.
    pub fn watch_subaddress(&mut self, account: u32, index: u32) -> Result<(), WalletError> {
        let index =
            SubaddressIndex::new(account, index).ok_or(WalletError::InvalidSubaddressIndex)?;
        self.scanner.register_subaddress(index);
        Ok(())
    }

    /// Scan a block fetched from a node.
    pub fn scan_block(&mut self, block: ScannableBlock) -> Result<Vec<Payment>, ScanError> {
        Ok(to_payments(self.scanner.scan(block)?))
    }

    /// Scan unconfirmed transactions, given as `(hash, transaction)` pairs.
    ///
    /// The result is only as trustworthy as the node that served the transactions: nothing here
    /// proves they are valid or will ever be mined.
    pub fn scan_mempool(
        &mut self,
        transactions: Vec<([u8; 32], Transaction<Pruned>)>,
    ) -> Result<Vec<Payment>, ScanError> {
        // The underlying scanner only accepts whole blocks, so wrap the transactions in a synthetic
        // one with an empty miner transaction.
        let (hashes, transactions): (Vec<_>, Vec<_>) = transactions.into_iter().unzip();
        let miner_transaction = Transaction::V2 {
            prefix: TransactionPrefix {
                additional_timelock: Timelock::None,
                inputs: vec![Input::Gen(0)],
                outputs: vec![],
                extra: vec![],
            },
            proofs: None,
        };
        let header = BlockHeader {
            hardfork_version: SUPPORTED_HARDFORK,
            hardfork_signal: SUPPORTED_HARDFORK,
            timestamp: 0,
            previous: [0; 32],
            nonce: 0,
        };
        let block = Block::new(header, miner_transaction, hashes).ok_or(ScanError::Malformed(
            "could not build the synthetic mempool block",
        ))?;

        self.scan_block(ScannableBlock {
            block,
            transactions,
            // Unconfirmed outputs have no position on the chain yet; this value is never exposed.
            output_index_for_first_ringct_output: Some(0),
        })
    }
}

fn to_payments(outputs: monero_wallet::Timelocked) -> Vec<Payment> {
    outputs
        .not_additionally_locked()
        .into_iter()
        .map(|output| Payment {
            txid: output.transaction(),
            output_index: output.index_in_transaction(),
            subaddress: output
                .subaddress()
                .map(|index| (index.account(), index.address())),
            amount: output.commitment().amount,
            output_key: output.key().compress().to_bytes(),
        })
        .collect()
}
