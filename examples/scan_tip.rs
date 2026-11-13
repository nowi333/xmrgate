//! Smoke test against a real node: scan the latest blocks and the mempool with a throwaway key.
use std::time::Instant;
use xmrgate::{Chain, Node, PaymentScanner, ViewWallet};

#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).expect("usage: scan_tip <node url>");
    // The well-known Monero donation address and its public view key.
    let wallet = ViewWallet::new(
    "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A",
    "f359631075708155cc3d92a32b75a7d02a5dcf27756707b47a2b31b21c389501",
  )
  .unwrap();
    let mut scanner = PaymentScanner::new(&wallet);
    for i in 1..=100 {
        scanner.watch_subaddress(0, i).unwrap();
    }

    let target = "888tNkZrPN6JsEgekjMnABU4TBzc2Dt29EPAvkRxbANsAnjyPbb3iQ1YBRk1UXcdRsiKc9dhwMVgN5S9cQUiyoogDavup3H";
    'search: for account in 0..100 {
        for index in 0..300 {
            if wallet.subaddress(account, index).ok().as_deref() == Some(target) {
                println!("donation subaddress is ({account}, {index})");
                break 'search;
            }
        }
    }

    let node = Node::new(&url, None).unwrap();
    let tip = node.tip().await.unwrap();
    println!(
        "tip {tip}, hash {}",
        hex::encode(node.block_hash(tip).await.unwrap())
    );
    for height in tip - 4..=tip {
        let start = Instant::now();
        let block = node.block(height).await.unwrap();
        let fetched = start.elapsed();
        let (txs, version) = (
            block.transactions.len(),
            block.block.header.hardfork_version,
        );
        let start = Instant::now();
        let found = scanner.scan_block(block).unwrap();
        println!(
            "block {height} v{version}: {txs} txs, fetch {fetched:?}, scan {:?}, {} payments",
            start.elapsed(),
            found.len()
        );
    }
    let pool = node.mempool_hashes().await.unwrap();
    let start = Instant::now();
    let txs = node.transactions(&pool).await.unwrap();
    let found = scanner
        .scan_mempool(pool.iter().copied().zip(txs).collect())
        .unwrap();
    println!(
        "mempool: {} txs in {:?}, {} payments",
        pool.len(),
        start.elapsed(),
        found.len()
    );
}
