//! Access to the Monero blockchain through remote nodes.

use std::time::Duration;

use async_trait::async_trait;
use monero_daemon_rpc::{HttpTransport, MoneroDaemon};
use monero_wallet::{
  interface::{
    InterfaceError, ProvidesBlockchain, ProvidesBlockchainMeta, ProvidesScannableBlocks,
    ProvidesTransactions, ScannableBlock,
  },
  transaction::{Pruned, Transaction},
};
use tokio::sync::Mutex;

/// A failure to obtain data from a node. Always treated as "try again or try another node".
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct ChainError(pub String);

impl From<InterfaceError> for ChainError {
  fn from(err: InterfaceError) -> Self {
    ChainError(err.to_string())
  }
}

/// A source of blocks and unconfirmed transactions.
#[async_trait]
pub trait Chain: Send + Sync {
  /// Height of the most recent block (the genesis block is 0).
  async fn tip(&self) -> Result<u64, ChainError>;
  /// Hash of the block at `height`.
  async fn block_hash(&self, height: u64) -> Result<[u8; 32], ChainError>;
  /// The block at `height` with its transactions, checked against the block's own hashes.
  async fn block(&self, height: u64) -> Result<ScannableBlock, ChainError>;
  /// Hashes of the transactions waiting in the mempool.
  async fn mempool_hashes(&self) -> Result<Vec<[u8; 32]>, ChainError>;
  /// Transactions by hash. Hashes the node no longer has are an error.
  async fn transactions(
    &self,
    hashes: &[[u8; 32]],
  ) -> Result<Vec<Transaction<Pruned>>, ChainError>;
}

#[derive(Clone)]
struct Transport {
  client: reqwest::Client,
  base: String,
}

impl HttpTransport for Transport {
  async fn post(
    &self,
    route: &str,
    body: Vec<u8>,
    response_size_limit: Option<usize>,
  ) -> Result<Vec<u8>, InterfaceError> {
    let err = |e: reqwest::Error| InterfaceError::InterfaceError(e.without_url().to_string());
    let mut response = self
      .client
      .post(format!("{}/{route}", self.base))
      .body(body)
      .send()
      .await
      .map_err(err)?
      .error_for_status()
      .map_err(err)?;

    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(err)? {
      out.extend_from_slice(&chunk);
      if response_size_limit.is_some_and(|limit| out.len() > limit) {
        return Err(InterfaceError::InvalidInterface("response exceeded size limit".into()));
      }
    }
    Ok(out)
  }
}

/// A single remote Monero node, reached over its RPC interface.
///
/// The connection is opened lazily, so a node that is down at startup is simply unhealthy.
pub struct Node {
  url: String,
  transport: Transport,
  daemon: Mutex<Option<MoneroDaemon<Transport>>>,
}

impl Node {
  /// `url` is the node's RPC address, such as `https://node.example.com:18089`.
  /// `socks5_proxy` routes the traffic through Tor when set, such as `127.0.0.1:9050`.
  pub fn new(url: &str, socks5_proxy: Option<&str>) -> Result<Self, ChainError> {
    let url = url.trim().trim_end_matches('/').to_owned();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
      return Err(ChainError("node URL must start with http:// or https://".into()));
    }
    let mut builder = reqwest::Client::builder()
      .timeout(Duration::from_secs(30))
      .connect_timeout(Duration::from_secs(15))
      .user_agent("xmrgate");
    if let Some(proxy) = socks5_proxy {
      // socks5h resolves hostnames through the proxy, which .onion addresses require.
      let proxy = reqwest::Proxy::all(format!("socks5h://{proxy}"))
        .map_err(|e| ChainError(format!("invalid proxy: {e}")))?;
      builder = builder.proxy(proxy);
    }
    let client = builder.build().map_err(|e| ChainError(e.to_string()))?;
    Ok(Self { transport: Transport { client, base: url.clone() }, url, daemon: Mutex::new(None) })
  }

  pub fn url(&self) -> &str {
    &self.url
  }

  async fn daemon(&self) -> Result<MoneroDaemon<Transport>, ChainError> {
    let mut slot = self.daemon.lock().await;
    if let Some(daemon) = slot.as_ref() {
      return Ok(daemon.clone());
    }
    let daemon = MoneroDaemon::new(self.transport.clone()).await?;
    *slot = Some(daemon.clone());
    Ok(daemon)
  }
}

fn parse_hash(hex_hash: &str) -> Result<[u8; 32], ChainError> {
  let mut hash = [0; 32];
  hex::decode_to_slice(hex_hash, &mut hash)
    .map_err(|_| ChainError("node returned a malformed hash".into()))?;
  Ok(hash)
}

#[async_trait]
impl Chain for Node {
  async fn tip(&self) -> Result<u64, ChainError> {
    Ok(self.daemon().await?.latest_block_number().await? as u64)
  }

  async fn block_hash(&self, height: u64) -> Result<[u8; 32], ChainError> {
    Ok(ProvidesBlockchain::block_hash(&self.daemon().await?, height as usize).await?)
  }

  async fn block(&self, height: u64) -> Result<ScannableBlock, ChainError> {
    Ok(self.daemon().await?.scannable_block_by_number(height as usize).await?)
  }

  async fn mempool_hashes(&self) -> Result<Vec<[u8; 32]>, ChainError> {
    #[derive(serde::Deserialize)]
    struct Pool {
      #[serde(default)]
      tx_hashes: Vec<String>,
    }
    let raw =
      self.daemon().await?.rpc_call("get_transaction_pool_hashes", None, 8 * 1024 * 1024).await?;
    let pool: Pool = serde_json::from_str(&raw)
      .map_err(|_| ChainError("node returned a malformed mempool listing".into()))?;
    pool.tx_hashes.iter().map(|hash| parse_hash(hash)).collect()
  }

  async fn transactions(
    &self,
    hashes: &[[u8; 32]],
  ) -> Result<Vec<Transaction<Pruned>>, ChainError> {
    if hashes.is_empty() {
      return Ok(vec![]);
    }
    self
      .daemon()
      .await?
      .pruned_transactions(hashes)
      .await
      .map_err(|e| ChainError(format!("{e:?}")))
  }
}
