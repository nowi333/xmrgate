//! Test helpers: a wallet, hand-built Monero outputs, and an in-memory chain.
//!
//! Outputs are built with an independent implementation of Monero's output derivation, straight
//! on curve25519-dalek, so the code under test is checked against the protocol rules.
#![allow(dead_code)]

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use async_trait::async_trait;
use curve25519_dalek::{constants::ED25519_BASEPOINT_TABLE as G, EdwardsPoint, Scalar};
use rand_core::{OsRng, RngCore};

use monero_wallet::{
    address::{AddressType, MoneroAddress},
    block::{Block, BlockHeader},
    ed25519::{Commitment, CompressedPoint, Point},
    interface::ScannableBlock,
    primitives::keccak256,
    ringct::{EncryptedAmount, PrunedRctProofs, RctBase, RctType},
    transaction::{Input, Output, Pruned, Timelock, Transaction, TransactionPrefix},
};
use xmrgate::{Chain, ChainError, Network, ViewWallet};

pub type Tx = Transaction<Pruned>;

pub fn random_scalar() -> Scalar {
    let mut bytes = [0u8; 64];
    OsRng.fill_bytes(&mut bytes);
    Scalar::from_bytes_mod_order_wide(&bytes)
}

pub fn random_hash() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

pub fn hash_to_scalar(data: &[u8]) -> Scalar {
    Scalar::from_bytes_mod_order(keccak256(data))
}

pub struct TestWallet {
    pub view: Scalar,
    pub spend_pub: EdwardsPoint,
    pub address: String,
    pub view_hex: String,
}

impl TestWallet {
    pub fn random() -> Self {
        let view = random_scalar();
        let spend_pub = &random_scalar() * G;
        let address = MoneroAddress::new(
            Network::Stagenet,
            AddressType::Legacy,
            Point::from(spend_pub),
            Point::from(&view * G),
        )
        .to_string();
        Self {
            view,
            spend_pub,
            address,
            view_hex: hex::encode(view.to_bytes()),
        }
    }

    pub fn load(&self) -> ViewWallet {
        ViewWallet::new(&self.address, &self.view_hex).unwrap()
    }

    /// `(spend, view)` public keys of a subaddress, per the Monero subaddress specification.
    pub fn subaddress_keys(&self, account: u32, index: u32) -> (EdwardsPoint, EdwardsPoint) {
        let mut data = b"SubAddr\0".to_vec();
        data.extend(self.view.to_bytes());
        data.extend(account.to_le_bytes());
        data.extend(index.to_le_bytes());
        let spend = self.spend_pub + &hash_to_scalar(&data) * G;
        (spend, self.view * spend)
    }

    pub fn primary_keys(&self) -> (EdwardsPoint, EdwardsPoint) {
        (self.spend_pub, &self.view * G)
    }
}

/// How the amount of a hand-built output is carried.
#[derive(Clone, Copy)]
pub enum AmountKind {
    /// In the clear, as in miner transactions.
    Clear,
    /// Encrypted with a commitment, as in every ordinary transaction.
    Encrypted,
}

/// Build a transaction with one output paying the given keys.
pub fn pay(
    (spend, view): (EdwardsPoint, EdwardsPoint),
    is_subaddress: bool,
    amount: u64,
    timelock: Timelock,
    kind: AmountKind,
) -> Tx {
    let r = random_scalar();
    let tx_pub = if is_subaddress { r * spend } else { &r * G };

    let mut derivation = (r * view).mul_by_cofactor().compress().to_bytes().to_vec();
    derivation.push(0); // varint of output index 0
    let view_tag = keccak256([b"view_tag".as_slice(), &derivation].concat())[0];
    let shared = hash_to_scalar(&derivation);
    let key = &shared * G + spend;

    let mut extra = vec![1u8];
    extra.extend(tx_pub.compress().to_bytes());

    let output = |amount| Output {
        amount,
        key: CompressedPoint::from(key.compress().to_bytes()),
        view_tag: Some(view_tag),
    };
    match kind {
        AmountKind::Clear => Transaction::V2 {
            prefix: TransactionPrefix {
                additional_timelock: timelock,
                inputs: vec![Input::Gen(0)],
                outputs: vec![output(Some(amount))],
                extra,
            },
            proofs: None,
        },
        AmountKind::Encrypted => {
            let shared_bytes = shared.to_bytes();
            let mask = hash_to_scalar(&[b"commitment_mask".as_slice(), &shared_bytes].concat());
            let pad = keccak256([b"amount".as_slice(), &shared_bytes].concat());
            let mut encrypted = amount.to_le_bytes();
            for (byte, pad) in encrypted.iter_mut().zip(pad) {
                *byte ^= pad;
            }
            let commitment = Commitment::new(monero_wallet::ed25519::Scalar::from(mask), amount)
                .commit()
                .compress();
            Transaction::V2 {
                prefix: TransactionPrefix {
                    additional_timelock: timelock,
                    inputs: vec![Input::ToKey {
                        amount: None,
                        key_offsets: vec![1, 2, 3],
                        key_image: CompressedPoint::from(
                            (&random_scalar() * G).compress().to_bytes(),
                        ),
                    }],
                    outputs: vec![output(None)],
                    extra,
                },
                proofs: Some(PrunedRctProofs {
                    rct_type: RctType::ClsagBulletproofPlus,
                    base: RctBase {
                        fee: 1000,
                        pseudo_outs: vec![],
                        encrypted_amounts: vec![EncryptedAmount::Compact { amount: encrypted }],
                        commitments: vec![commitment],
                    },
                }),
            }
        }
    }
}

/// Build an ordinary (encrypted-amount) payment to an address given as text.
pub fn pay_address(address: &str, amount: u64) -> Tx {
    let address = MoneroAddress::from_str(Network::Stagenet, address).unwrap();
    pay(
        (address.spend().into(), address.view().into()),
        address.is_subaddress(),
        amount,
        Timelock::None,
        AmountKind::Encrypted,
    )
}

#[derive(Default)]
struct ChainState {
    blocks: Vec<ScannableBlock>,
    mempool: Vec<([u8; 32], Tx)>,
    down: bool,
    /// Report this height instead of the real one.
    claimed_tip: Option<u64>,
}

/// An in-memory chain that tests drive by hand.
#[derive(Default)]
pub struct MockChain {
    state: Mutex<ChainState>,
    pub blocks_served: AtomicU64,
}

impl MockChain {
    /// A chain holding `length` empty blocks.
    pub fn new(length: usize) -> Self {
        let chain = Self::default();
        for _ in 0..length {
            chain.mine(vec![]);
        }
        chain
    }

    pub fn height(&self) -> u64 {
        self.state.lock().unwrap().blocks.len() as u64 - 1
    }

    /// Mine a block holding `txs`, removing them from the mempool. Returns the block.
    pub fn mine(&self, txs: Vec<([u8; 32], Tx)>) -> ScannableBlock {
        let mut state = self.state.lock().unwrap();
        let previous = state
            .blocks
            .last()
            .map(|block| block.block.hash())
            .unwrap_or([0; 32]);
        let miner = Transaction::V2 {
            prefix: TransactionPrefix {
                additional_timelock: Timelock::None,
                inputs: vec![Input::Gen(state.blocks.len())],
                outputs: vec![],
                extra: vec![],
            },
            proofs: None,
        };
        let header = BlockHeader {
            hardfork_version: 16,
            hardfork_signal: 16,
            timestamp: 0,
            previous,
            nonce: OsRng.next_u32(),
        };
        state
            .mempool
            .retain(|(hash, _)| !txs.iter().any(|(mined, _)| mined == hash));
        let (hashes, transactions): (Vec<_>, Vec<_>) = txs.into_iter().unzip();
        let block = ScannableBlock {
            block: Block::new(header, miner, hashes).unwrap(),
            transactions,
            output_index_for_first_ringct_output: Some(1),
        };
        state.blocks.push(block.clone());
        block
    }

    /// Mine a block holding everything currently in the mempool.
    pub fn mine_mempool(&self) -> ScannableBlock {
        let txs = self.state.lock().unwrap().mempool.clone();
        self.mine(txs)
    }

    /// Append a block mined elsewhere, to keep several mock nodes on the same chain.
    pub fn accept(&self, block: ScannableBlock) {
        self.state.lock().unwrap().blocks.push(block);
    }

    /// Put a transaction in the mempool and return its hash.
    pub fn broadcast(&self, tx: Tx) -> [u8; 32] {
        let hash = random_hash();
        self.state.lock().unwrap().mempool.push((hash, tx));
        hash
    }

    /// Remove a transaction from the mempool without mining it.
    pub fn evict(&self, hash: [u8; 32]) {
        self.state
            .lock()
            .unwrap()
            .mempool
            .retain(|(h, _)| *h != hash);
    }

    /// Drop the last `depth` blocks, as a reorganisation would.
    pub fn orphan(&self, depth: usize) {
        let mut state = self.state.lock().unwrap();
        let keep = state.blocks.len() - depth;
        state.blocks.truncate(keep);
    }

    pub fn set_down(&self, down: bool) {
        self.state.lock().unwrap().down = down;
    }

    pub fn claim_tip(&self, tip: Option<u64>) {
        self.state.lock().unwrap().claimed_tip = tip;
    }

    fn up(&self) -> Result<std::sync::MutexGuard<'_, ChainState>, ChainError> {
        let state = self.state.lock().unwrap();
        if state.down {
            return Err(ChainError("connection refused".into()));
        }
        Ok(state)
    }
}

#[async_trait]
impl Chain for MockChain {
    async fn tip(&self) -> Result<u64, ChainError> {
        let state = self.up()?;
        Ok(state.claimed_tip.unwrap_or(state.blocks.len() as u64 - 1))
    }

    async fn block_hash(&self, height: u64) -> Result<[u8; 32], ChainError> {
        self.up()?
            .blocks
            .get(height as usize)
            .map(|block| block.block.hash())
            .ok_or(ChainError("no such block".into()))
    }

    async fn block(&self, height: u64) -> Result<ScannableBlock, ChainError> {
        self.blocks_served.fetch_add(1, Ordering::SeqCst);
        self.up()?
            .blocks
            .get(height as usize)
            .cloned()
            .ok_or(ChainError("no such block".into()))
    }

    async fn mempool_hashes(&self) -> Result<Vec<[u8; 32]>, ChainError> {
        Ok(self.up()?.mempool.iter().map(|(hash, _)| *hash).collect())
    }

    async fn transactions(&self, hashes: &[[u8; 32]]) -> Result<Vec<Tx>, ChainError> {
        let state = self.up()?;
        hashes
            .iter()
            .map(|hash| {
                state
                    .mempool
                    .iter()
                    .find(|(h, _)| h == hash)
                    .map(|(_, tx)| tx.clone())
                    .ok_or(ChainError("transaction not found".into()))
            })
            .collect()
    }
}
