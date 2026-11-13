//! Checks against the real Monero network. They need internet access, so they are skipped by
//! default. Run them with: `cargo test --test mainnet -- --ignored`
//!
//! The wallet is the Monero project's donation wallet, whose view key is public.

use xmrgate::{Chain, Node, PaymentScanner, ViewWallet};

const NODE: &str = "https://node.sethforprivacy.com";

fn donation_scanner() -> PaymentScanner {
  let wallet = ViewWallet::new(
    "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A",
    "f359631075708155cc3d92a32b75a7d02a5dcf27756707b47a2b31b21c389501",
  )
  .unwrap();
  let mut scanner = PaymentScanner::new(&wallet);
  scanner.watch_subaddress(0, 70).unwrap();
  scanner
}

#[tokio::test]
#[ignore = "needs internet access"]
async fn finds_a_real_donation_with_its_encrypted_amount() {
  let node = Node::new(NODE, None).unwrap();
  let block = node.block(3_802_838).await.unwrap();
  let payments = donation_scanner().scan_block(block).unwrap();
  assert_eq!(payments.len(), 1);
  assert_eq!(
    hex::encode(payments[0].txid),
    "f5acb60819595a94901a074ac078f69b1df9ee96503ac427ef2fdde79637a779"
  );
  assert_eq!(payments[0].output_index, 1);
  assert_eq!(payments[0].subaddress, Some((0, 70)));
  assert_eq!(payments[0].amount, 43_635_936_122);
}

#[tokio::test]
#[ignore = "needs internet access"]
async fn reads_the_tip_and_the_mempool() {
  let node = Node::new(NODE, None).unwrap();
  let tip = node.tip().await.unwrap();
  assert!(tip > 3_800_000);
  assert_eq!(node.block(tip - 1).await.unwrap().block.hash(), node.block_hash(tip - 1).await.unwrap());
  let pool = node.mempool_hashes().await.unwrap();
  let transactions = node.transactions(&pool).await.unwrap();
  assert_eq!(transactions.len(), pool.len());
  donation_scanner().scan_mempool(pool.into_iter().zip(transactions).collect()).unwrap();
}
