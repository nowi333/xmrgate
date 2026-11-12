//! xmrgate: accept Monero with a primary address and a private view key.
//!
//! The view key never leaves this process. Blocks and mempool transactions are fetched from
//! remote nodes and scanned locally, so nodes learn nothing about the wallet.

pub mod amount;
mod chain;
mod gateway;
mod pool;
pub mod rates;
mod scan;
pub mod server;
mod store;
mod wallet;

pub use chain::{Chain, ChainError, Node};
pub use gateway::{invoice_json, sign, Gateway, GatewayError, Health, NewInvoice};
pub use monero_wallet::address::Network;
pub use pool::{NodePool, NodeStatus};
pub use scan::{Payment, PaymentScanner, ScanError};
pub use store::{Button, Event, Invoice, Site, Status, Store, StoreError, StoredPayment};
pub use wallet::{ViewWallet, WalletError};
