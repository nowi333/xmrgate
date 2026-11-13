//! A set of remote nodes treated as one untrusted source.
//!
//! No single node is believed on its own: the height must be reached by two nodes, and every
//! block is accepted only once a second node reports the same hash for that height. A block's
//! hash commits to all of its transactions, so agreeing on the hash is agreeing on the content.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::future::join_all;
use monero_wallet::{
    interface::ScannableBlock,
    transaction::{Pruned, Transaction},
};

use crate::chain::{Chain, ChainError};

/// A node is healthy while its height is within this many blocks of the agreed height.
const MAX_LAG: u64 = 3;
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// A node caught serving a block the other nodes contradict is set aside for this long.
const DISTRUST: Duration = Duration::from_secs(30 * 60);

/// What is known about one node after the latest probe.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct NodeStatus {
    pub url: String,
    pub healthy: bool,
    pub height: Option<u64>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

pub struct NodePool {
    nodes: Vec<(String, Arc<dyn Chain>)>,
    status: Mutex<Vec<NodeStatus>>,
    preferred: AtomicUsize,
    notices: Mutex<Vec<String>>,
    distrusted_at: Mutex<Vec<Option<Instant>>>,
}

impl NodePool {
    /// `nodes` are `(label, node)` pairs. With a single node there is nothing to cross-check
    /// against, and that node is trusted.
    pub fn new(nodes: Vec<(String, Arc<dyn Chain>)>) -> Self {
        let status = nodes
            .iter()
            .map(|(url, _)| NodeStatus {
                url: url.clone(),
                ..Default::default()
            })
            .collect();
        Self {
            distrusted_at: Mutex::new(vec![None; nodes.len()]),
            nodes,
            status: Mutex::new(status),
            preferred: AtomicUsize::new(0),
            notices: Mutex::new(vec![]),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The state of each node as of the latest height check.
    pub fn status(&self) -> Vec<NodeStatus> {
        self.status.lock().unwrap().clone()
    }

    /// Notable events (failovers, disagreements) since the last call.
    pub fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.lock().unwrap())
    }

    fn notice(&self, message: String) {
        let mut notices = self.notices.lock().unwrap();
        if notices.len() < 100 && notices.last() != Some(&message) {
            notices.push(message);
        }
    }

    fn distrusted(&self, index: usize) -> bool {
        self.distrusted_at.lock().unwrap()[index].is_some_and(|at| at.elapsed() < DISTRUST)
    }

    /// Healthy nodes, the preferred one first, then by latency.
    fn healthy(&self) -> Vec<usize> {
        let status = self.status.lock().unwrap();
        let preferred = self.preferred.load(Ordering::Relaxed);
        let mut order: Vec<usize> = (0..self.nodes.len())
            .filter(|i| status[*i].healthy && !self.distrusted(*i))
            .collect();
        order.sort_by_key(|i| (*i != preferred, status[*i].latency_ms.unwrap_or(u64::MAX)));
        order
    }

    fn mark_failed(&self, index: usize, error: &ChainError) {
        let mut status = self.status.lock().unwrap();
        if status[index].healthy {
            status[index].healthy = false;
            status[index].error = Some(error.0.clone());
            drop(status);
            self.notice(format!("node {} failed: {}", self.nodes[index].0, error.0));
        }
    }

    /// Run `call` on healthy nodes in order until one succeeds.
    async fn first_ok<T, F, Fut>(&self, call: F) -> Result<(usize, T), ChainError>
    where
        F: Fn(Arc<dyn Chain>) -> Fut,
        Fut: std::future::Future<Output = Result<T, ChainError>>,
    {
        let mut last = ChainError("no healthy node available".into());
        for index in self.healthy() {
            match call(self.nodes[index].1.clone()).await {
                Ok(value) => {
                    if self.preferred.swap(index, Ordering::Relaxed) != index {
                        self.notice(format!("now using node {}", self.nodes[index].0));
                    }
                    return Ok((index, value));
                }
                Err(e) => {
                    self.mark_failed(index, &e);
                    last = e;
                }
            }
        }
        Err(last)
    }
}

#[async_trait]
impl Chain for NodePool {
    /// Probe every node and return the highest height reached by at least two of them.
    async fn tip(&self) -> Result<u64, ChainError> {
        let probes = join_all(self.nodes.iter().map(|(_, node)| async move {
            let start = Instant::now();
            let result = tokio::time::timeout(PROBE_TIMEOUT, node.tip())
                .await
                .unwrap_or_else(|_| Err(ChainError("timed out".into())));
            (result, start.elapsed().as_millis() as u64)
        }))
        .await;

        let mut heights: Vec<u64> = probes
            .iter()
            .filter_map(|(r, _)| r.as_ref().ok().copied())
            .collect();
        heights.sort_unstable_by(|a, b| b.cmp(a));
        let agreed = match heights.len() {
            0 => None,
            1 => Some(heights[0]),
            _ => Some(heights[1]),
        };

        let mut status = self.status.lock().unwrap();
        for (i, (result, latency)) in probes.into_iter().enumerate() {
            let entry = &mut status[i];
            match result {
                Ok(height) => {
                    entry.height = Some(height);
                    entry.latency_ms = Some(latency);
                    let in_step = agreed.is_some_and(|agreed| height + MAX_LAG >= agreed);
                    let distrusted = self.distrusted(i);
                    entry.healthy = in_step && !distrusted;
                    entry.error = if distrusted {
                        Some("served a block the other nodes contradict".to_owned())
                    } else if !in_step {
                        Some("behind the other nodes".to_owned())
                    } else {
                        None
                    };
                }
                Err(e) => {
                    entry.healthy = false;
                    entry.latency_ms = None;
                    entry.error = Some(e.0);
                }
            }
        }
        agreed.ok_or_else(|| ChainError("no node is reachable".into()))
    }

    async fn block_hash(&self, height: u64) -> Result<[u8; 32], ChainError> {
        Ok(self
            .first_ok(|node| async move { node.block_hash(height).await })
            .await?
            .1)
    }

    async fn block(&self, height: u64) -> Result<ScannableBlock, ChainError> {
        let (source, block) = self
            .first_ok(|node| async move { node.block(height).await })
            .await?;
        if self.nodes.len() < 2 {
            return Ok(block);
        }

        // Ask the other healthy nodes, one at a time, until one of them confirms the hash.
        let hash = block.block.hash();
        let mut asked = 0;
        for witness in self.healthy().into_iter().filter(|i| *i != source) {
            match self.nodes[witness].1.block_hash(height).await {
                Ok(other) if other == hash => return Ok(block),
                Ok(_) => {
                    asked += 1;
                    self.notice(format!(
                        "nodes {} and {} disagree on block {height}",
                        self.nodes[source].0, self.nodes[witness].0
                    ));
                }
                Err(e) => self.mark_failed(witness, &e),
            }
        }

        if asked > 0 {
            // Other nodes contradict this block and none vouches for it: set its source aside.
            self.distrusted_at.lock().unwrap()[source] = Some(Instant::now());
            self.mark_failed(
                source,
                &ChainError(format!("served a contradicted block {height}")),
            );
        }
        Err(ChainError(format!(
            "block {height} could not be confirmed by a second node"
        )))
    }

    async fn mempool_hashes(&self) -> Result<Vec<[u8; 32]>, ChainError> {
        Ok(self
            .first_ok(|node| async move { node.mempool_hashes().await })
            .await?
            .1)
    }

    async fn transactions(
        &self,
        hashes: &[[u8; 32]],
    ) -> Result<Vec<Transaction<Pruned>>, ChainError> {
        // Not failed over: a transaction leaving the mempool between two calls is not a node fault.
        let Some(index) = self.healthy().into_iter().next() else {
            return Err(ChainError("no healthy node available".into()));
        };
        self.nodes[index].1.transactions(hashes).await
    }
}
