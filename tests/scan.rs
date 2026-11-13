//! Tests of key handling and scanning.

mod common;

use monero_wallet::{
    address::{AddressType, MoneroAddress},
    block::{Block, BlockHeader},
    ed25519::Point,
    interface::ScannableBlock,
    transaction::{Input, Pruned, Timelock, Transaction, TransactionPrefix},
};
use xmrgate::{Network, PaymentScanner, ViewWallet, WalletError};

use common::{AmountKind, TestWallet};

fn pay(
    keys: (
        curve25519_dalek::EdwardsPoint,
        curve25519_dalek::EdwardsPoint,
    ),
    is_subaddress: bool,
    amount: u64,
    timelock: Timelock,
) -> Transaction<Pruned> {
    common::pay(keys, is_subaddress, amount, timelock, AmountKind::Clear)
}

fn block_with(hardfork: u8, txs: Vec<([u8; 32], Transaction<Pruned>)>) -> ScannableBlock {
    let (hashes, transactions): (Vec<_>, Vec<_>) = txs.into_iter().unzip();
    let miner = Transaction::V2 {
        prefix: TransactionPrefix {
            additional_timelock: Timelock::None,
            inputs: vec![Input::Gen(1234)],
            outputs: vec![],
            extra: vec![],
        },
        proofs: None,
    };
    let header = BlockHeader {
        hardfork_version: hardfork,
        hardfork_signal: hardfork,
        timestamp: 0,
        previous: [0; 32],
        nonce: 0,
    };
    ScannableBlock {
        block: Block::new(header, miner, hashes).unwrap(),
        transactions,
        output_index_for_first_ringct_output: Some(1000),
    }
}

#[test]
fn accepts_matching_address_and_view_key() {
    let w = TestWallet::random();
    let wallet = w.load();
    assert_eq!(wallet.network(), Network::Stagenet);
    assert_eq!(wallet.primary_address(), w.address);
    // Surrounding whitespace from a copy-paste is tolerated.
    assert!(ViewWallet::new(&format!(" {}\n", w.address), &format!("{} ", w.view_hex)).is_ok());
}

#[test]
fn rejects_bad_credentials() {
    let w = TestWallet::random();
    let other = TestWallet::random();
    let err = |addr: &str, key: &str| ViewWallet::new(addr, key).err().unwrap();

    assert_eq!(
        err(&w.address, &other.view_hex),
        WalletError::ViewKeyMismatch
    );
    assert_eq!(
        err("not an address", &w.view_hex),
        WalletError::InvalidAddress
    );
    assert_eq!(err(&w.address, "abcd"), WalletError::InvalidViewKey);
    assert_eq!(
        err(&w.address, &"ff".repeat(32)),
        WalletError::InvalidViewKey
    );

    let subaddress = w.load().subaddress(0, 1).unwrap();
    assert_eq!(
        err(&subaddress, &w.view_hex),
        WalletError::NotPrimaryAddress
    );
}

#[test]
fn derives_subaddresses_per_specification() {
    let w = TestWallet::random();
    let wallet = w.load();
    for (account, index) in [(0, 1), (0, 2), (0, 500_000), (3, 0), (7, 42)] {
        let (spend, view) = w.subaddress_keys(account, index);
        let expected = MoneroAddress::new(
            Network::Stagenet,
            AddressType::Subaddress,
            Point::from(spend),
            Point::from(view),
        )
        .to_string();
        assert_eq!(wallet.subaddress(account, index).unwrap(), expected);
    }
    assert_eq!(
        wallet.subaddress(0, 0),
        Err(WalletError::InvalidSubaddressIndex)
    );
}

#[test]
fn detects_mempool_payment_to_watched_subaddress() {
    let w = TestWallet::random();
    let mut scanner = PaymentScanner::new(&w.load());
    scanner.watch_subaddress(0, 5).unwrap();

    let to_watched = pay(
        w.subaddress_keys(0, 5),
        true,
        123_000_000_000,
        Timelock::None,
    );
    let to_unwatched = pay(w.subaddress_keys(0, 6), true, 1, Timelock::None);
    let to_primary = pay(w.primary_keys(), false, 77, Timelock::None);
    let to_stranger = pay(
        TestWallet::random().primary_keys(),
        false,
        9,
        Timelock::None,
    );

    let payments = scanner
        .scan_mempool(vec![
            ([1; 32], to_stranger),
            ([2; 32], to_watched.clone()),
            ([3; 32], to_unwatched),
            ([4; 32], to_primary),
        ])
        .unwrap();

    assert_eq!(payments.len(), 2);
    assert_eq!(payments[0].txid, [2; 32]);
    assert_eq!(payments[0].subaddress, Some((0, 5)));
    assert_eq!(payments[0].amount, 123_000_000_000);
    assert_eq!(payments[0].output_index, 0);
    assert_eq!(
        payments[0].output_key,
        to_watched.prefix().outputs[0].key.to_bytes()
    );
    assert_eq!(payments[1].txid, [4; 32]);
    assert_eq!(payments[1].subaddress, None);
    assert_eq!(payments[1].amount, 77);

    assert!(scanner.scan_mempool(vec![]).unwrap().is_empty());
}

#[test]
fn detects_payment_in_block() {
    let w = TestWallet::random();
    let mut scanner = PaymentScanner::new(&w.load());
    scanner.watch_subaddress(2, 9).unwrap();

    let tx = pay(w.subaddress_keys(2, 9), true, 5_000, Timelock::None);
    let payments = scanner
        .scan_block(block_with(16, vec![([8; 32], tx)]))
        .unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].subaddress, Some((2, 9)));
    assert_eq!(payments[0].amount, 5_000);
}

#[test]
fn ignores_timelocked_outputs() {
    let w = TestWallet::random();
    let mut scanner = PaymentScanner::new(&w.load());
    let tx = pay(w.primary_keys(), false, 5_000, Timelock::Block(usize::MAX));
    assert!(scanner
        .scan_mempool(vec![([8; 32], tx)])
        .unwrap()
        .is_empty());
}

#[test]
fn refuses_unknown_protocol_versions() {
    let w = TestWallet::random();
    let mut scanner = PaymentScanner::new(&w.load());
    let tx = pay(w.primary_keys(), false, 5_000, Timelock::None);
    assert_eq!(
        scanner.scan_block(block_with(17, vec![([8; 32], tx)])),
        Err(xmrgate::ScanError::UnsupportedProtocol(17)),
    );
}

#[test]
fn decrypts_amounts_and_rejects_forged_commitments() {
    let w = TestWallet::random();
    let mut scanner = PaymentScanner::new(&w.load());
    scanner.watch_subaddress(0, 1).unwrap();

    let honest = common::pay(
        w.subaddress_keys(0, 1),
        true,
        250_000_000_000,
        Timelock::None,
        AmountKind::Encrypted,
    );
    // The same payment with a commitment that does not match the encrypted amount.
    let mut forged = common::pay(
        w.subaddress_keys(0, 1),
        true,
        250_000_000_000,
        Timelock::None,
        AmountKind::Encrypted,
    );
    let other = common::pay(
        w.subaddress_keys(0, 1),
        true,
        999_000_000_000,
        Timelock::None,
        AmountKind::Encrypted,
    );
    let (
        Transaction::V2 {
            proofs: Some(forged_proofs),
            ..
        },
        Transaction::V2 {
            proofs: Some(other_proofs),
            ..
        },
    ) = (&mut forged, &other)
    else {
        unreachable!()
    };
    forged_proofs.base.commitments = other_proofs.base.commitments.clone();

    let payments = scanner
        .scan_mempool(vec![([1; 32], honest), ([2; 32], forged)])
        .unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].txid, [1; 32]);
    assert_eq!(payments[0].amount, 250_000_000_000);
}

/// The Monero project's public donation wallet: a real-world check of address handling and
/// subaddress derivation against values produced by the official wallet.
#[test]
fn matches_the_public_donation_wallet() {
    let wallet = ViewWallet::new(
    "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A",
    "f359631075708155cc3d92a32b75a7d02a5dcf27756707b47a2b31b21c389501",
  )
  .unwrap();
    assert_eq!(wallet.network(), Network::Mainnet);
    assert_eq!(
    wallet.subaddress(0, 70).unwrap(),
    "888tNkZrPN6JsEgekjMnABU4TBzc2Dt29EPAvkRxbANsAnjyPbb3iQ1YBRk1UXcdRsiKc9dhwMVgN5S9cQUiyoogDavup3H",
  );
}
