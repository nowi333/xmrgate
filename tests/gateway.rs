//! Tests of the payment engine against an in-memory chain.

mod common;

use std::sync::{atomic::Ordering, Arc};

use common::{pay_address, MockChain, TestWallet};
use xmrgate::{Chain, Gateway, Invoice, NewInvoice, NodePool, Status, Store};

const XMR: u64 = 1_000_000_000_000;
const T0: i64 = 1_800_000_000;

struct Setup {
    chain: Arc<MockChain>,
    store: Arc<Store>,
    gateway: Gateway,
    wallet: TestWallet,
}

fn setup() -> Setup {
    let chain = Arc::new(MockChain::new(10));
    let store = Arc::new(Store::in_memory().unwrap());
    let wallet = TestWallet::random();
    let gateway = Gateway::new(store.clone(), wallet.load(), chain.clone()).unwrap();
    Setup {
        chain,
        store,
        gateway,
        wallet,
    }
}

fn new_invoice(amount: u64, confirmations: u32) -> NewInvoice {
    NewInvoice {
        amount,
        confirmations,
        ttl_secs: 3600,
        order_ref: Some("order-1".into()),
        callback_url: Some("http://shop.invalid/hook".into()),
        ..Default::default()
    }
}

impl Setup {
    async fn tick(&self, now: i64) {
        while self.gateway.tick(now).await.unwrap() {}
    }

    fn invoice(&self, id: &str) -> Invoice {
        self.store.invoice(id).unwrap().unwrap()
    }

    fn webhook_statuses(&self) -> Vec<String> {
        self.store
            .due_webhooks(i64::MAX - 1)
            .unwrap()
            .iter()
            .map(|hook| {
                let json: serde_json::Value = serde_json::from_str(&hook.payload).unwrap();
                json["invoice"]["status"].as_str().unwrap().to_owned()
            })
            .collect()
    }
}

#[tokio::test]
async fn mempool_payment_is_seen_before_it_is_mined() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s
        .gateway
        .create_invoice(new_invoice(2 * XMR, 1), T0)
        .unwrap();
    assert_eq!(invoice.sub_index, 1);
    assert_eq!(invoice.address, s.wallet.load().subaddress(0, 1).unwrap());

    s.tick(T0 + 1).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Pending);

    let txid = s.chain.broadcast(pay_address(&invoice.address, 2 * XMR));
    s.tick(T0 + 2).await;
    let seen = s.invoice(&invoice.id);
    assert_eq!(
        (seen.status, seen.received, seen.confirmations),
        (Status::Seen, 2 * XMR, 0)
    );

    let _ = txid;
}

#[tokio::test]
async fn mined_payment_confirms_and_notifies_once_per_status() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s
        .gateway
        .create_invoice(new_invoice(2 * XMR, 1), T0)
        .unwrap();

    let tx = pay_address(&invoice.address, 2 * XMR);
    let txid = s.chain.broadcast(tx.clone());
    s.tick(T0 + 2).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Seen);

    s.chain.mine(vec![(txid, tx)]);
    s.tick(T0 + 3).await;
    s.tick(T0 + 4).await;
    let confirmed = s.invoice(&invoice.id);
    assert_eq!(
        (confirmed.status, confirmed.confirmations),
        (Status::Confirmed, 1)
    );
    assert_eq!(s.webhook_statuses(), ["seen", "confirmed"]);
}

#[tokio::test]
async fn waits_for_the_required_confirmations() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 3), T0).unwrap();

    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 1).await;
    assert_eq!(
        (
            s.invoice(&invoice.id).status,
            s.invoice(&invoice.id).confirmations
        ),
        (Status::Seen, 1)
    );
    s.chain.mine(vec![]);
    s.tick(T0 + 2).await;
    assert_eq!(
        (
            s.invoice(&invoice.id).status,
            s.invoice(&invoice.id).confirmations
        ),
        (Status::Seen, 2)
    );
    s.chain.mine(vec![]);
    s.tick(T0 + 3).await;
    assert_eq!(
        (
            s.invoice(&invoice.id).status,
            s.invoice(&invoice.id).confirmations
        ),
        (Status::Confirmed, 3)
    );
}

#[tokio::test]
async fn zero_confirmations_accepts_mempool_payments() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 0), T0).unwrap();
    s.chain.broadcast(pay_address(&invoice.address, XMR));
    s.tick(T0 + 1).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);
}

#[tokio::test]
async fn partial_payment_is_underpaid_until_topped_up() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s
        .gateway
        .create_invoice(new_invoice(3 * XMR, 1), T0)
        .unwrap();

    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 1).await;
    let partial = s.invoice(&invoice.id);
    assert_eq!((partial.status, partial.received), (Status::Underpaid, XMR));

    // Still underpaid after expiry: money was received, so it is not simply "expired".
    s.tick(T0 + 4000).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Underpaid);

    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, 2 * XMR),
    )]);
    s.tick(T0 + 4001).await;
    let full = s.invoice(&invoice.id);
    assert_eq!((full.status, full.received), (Status::Confirmed, 3 * XMR));
    assert_eq!(s.webhook_statuses(), ["underpaid", "confirmed"]);
}

#[tokio::test]
async fn payments_to_other_invoices_and_wallets_are_not_credited() {
    let s = setup();
    s.tick(T0).await;
    let a = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    let b = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    assert_ne!(a.address, b.address);

    let stranger = TestWallet::random().load().subaddress(0, 1).unwrap();
    s.chain.mine(vec![
        (common::random_hash(), pay_address(&b.address, XMR)),
        (common::random_hash(), pay_address(&stranger, XMR)),
        (common::random_hash(), pay_address(&s.wallet.address, XMR)),
    ]);
    s.tick(T0 + 1).await;
    assert_eq!(s.invoice(&a.id).status, Status::Pending);
    assert_eq!(s.invoice(&b.id).status, Status::Confirmed);
}

#[tokio::test]
async fn unpaid_invoice_expires_and_its_subaddress_is_reused_after_a_day() {
    let s = setup();
    s.tick(T0).await;
    let first = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    s.tick(T0 + 3599).await;
    assert_eq!(s.invoice(&first.id).status, Status::Pending);
    s.tick(T0 + 3600).await;
    assert_eq!(s.invoice(&first.id).status, Status::Expired);
    assert_eq!(s.webhook_statuses(), ["expired"]);

    // Within the grace period the subaddress is still held, and a late payment is honoured.
    let second = s
        .gateway
        .create_invoice(new_invoice(XMR, 1), T0 + 7200)
        .unwrap();
    assert_eq!(second.sub_index, 2);

    s.tick(T0 + 3600 + 24 * 3600 + 1).await;
    let third = s
        .gateway
        .create_invoice(new_invoice(XMR, 1), T0 + 90_001)
        .unwrap();
    assert_eq!(
        third.sub_index, 1,
        "the expired invoice's subaddress is free again"
    );

    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&third.address, XMR),
    )]);
    s.tick(T0 + 90_002).await;
    assert_eq!(s.invoice(&third.id).status, Status::Confirmed);
    assert_eq!(s.invoice(&first.id).status, Status::Expired);
}

#[tokio::test]
async fn late_payment_within_the_grace_period_is_honoured() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    s.tick(T0 + 3700).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Expired);

    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 5000).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);
    // A paid invoice keeps its subaddress for good.
    s.tick(T0 + 10 * 24 * 3600).await;
    let next = s
        .gateway
        .create_invoice(new_invoice(XMR, 1), T0 + 10 * 24 * 3600)
        .unwrap();
    assert_eq!(next.sub_index, 2);
}

#[tokio::test]
async fn reorganisation_demotes_a_confirmed_invoice_and_remining_restores_it() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    let tx = pay_address(&invoice.address, XMR);
    let txid = common::random_hash();

    s.chain.mine(vec![(txid, tx.clone())]);
    s.tick(T0 + 1).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);

    // The block is orphaned and replaced by an empty one.
    s.chain.orphan(1);
    s.chain.mine(vec![]);
    s.tick(T0 + 2).await;
    let demoted = s.invoice(&invoice.id);
    assert_eq!((demoted.status, demoted.confirmations), (Status::Seen, 0));

    // The transaction is mined again in a later block.
    s.chain.mine(vec![(txid, tx)]);
    s.tick(T0 + 3).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);
    assert_eq!(s.webhook_statuses(), ["confirmed", "seen", "confirmed"]);
    assert!(s
        .store
        .recent_events(50)
        .unwrap()
        .iter()
        .any(|event| event.kind == "reorg"));
}

#[tokio::test]
async fn payment_lost_in_a_reorganisation_is_dropped_after_ten_minutes() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 1).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);

    s.chain.orphan(1);
    s.chain.mine(vec![]);
    s.chain.mine(vec![]);
    s.tick(T0 + 2).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Seen);
    s.tick(T0 + 300).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Seen);
    s.tick(T0 + 700).await;
    let dropped = s.invoice(&invoice.id);
    assert_eq!((dropped.status, dropped.received), (Status::Pending, 0));
}

#[tokio::test]
async fn deep_reorganisation_is_followed() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    for _ in 0..5 {
        s.chain.mine(vec![]);
    }
    s.tick(T0 + 1).await;

    s.chain.orphan(4);
    for _ in 0..3 {
        s.chain.mine(vec![]);
    }
    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.chain.mine(vec![]);
    s.tick(T0 + 2).await;
    let paid = s.invoice(&invoice.id);
    assert_eq!((paid.status, paid.confirmations), (Status::Confirmed, 2));
}

#[tokio::test]
async fn mempool_payment_that_vanishes_is_dropped() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();
    let txid = s.chain.broadcast(pay_address(&invoice.address, XMR));
    s.tick(T0 + 1).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Seen);

    s.chain.evict(txid);
    s.tick(T0 + 2).await;
    assert_eq!(
        s.invoice(&invoice.id).status,
        Status::Seen,
        "a brief absence is tolerated"
    );
    s.tick(T0 + 700).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Pending);
    assert_eq!(s.webhook_statuses(), ["seen", "pending"]);
}

#[tokio::test]
async fn duplicate_output_key_is_counted_once() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s
        .gateway
        .create_invoice(new_invoice(2 * XMR, 1), T0)
        .unwrap();

    // The burning bug: the same one-time key paid twice. Only one of the two can ever be spent.
    let tx = pay_address(&invoice.address, XMR);
    s.chain.mine(vec![(common::random_hash(), tx.clone())]);
    s.chain.mine(vec![(common::random_hash(), tx)]);
    s.tick(T0 + 1).await;
    let result = s.invoice(&invoice.id);
    assert_eq!((result.status, result.received), (Status::Underpaid, XMR));
}

#[tokio::test]
async fn nothing_is_downloaded_while_no_invoice_is_open() {
    let s = setup();
    for i in 0..5 {
        s.chain.mine(vec![]);
        s.tick(T0 + i).await;
    }
    assert_eq!(s.chain.blocks_served.load(Ordering::SeqCst), 0);
    assert!(s.gateway.health().idle);
    assert_eq!(s.gateway.health().scanned, Some(s.chain.height()));

    // Scanning starts with the first invoice, from the current tip.
    let invoice = s
        .gateway
        .create_invoice(new_invoice(XMR, 1), T0 + 10)
        .unwrap();
    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 11).await;
    assert_eq!(s.chain.blocks_served.load(Ordering::SeqCst), 1);
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);
}

#[tokio::test]
async fn restart_resumes_and_catches_up_on_missed_blocks() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 2), T0).unwrap();
    s.tick(T0 + 1).await;

    // The gateway is stopped. A payment is mined, followed by many blocks.
    let Setup {
        chain,
        store,
        wallet,
        gateway,
    } = s;
    drop(gateway);
    chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    for _ in 0..70 {
        chain.mine(vec![]);
    }

    let gateway = Gateway::new(store.clone(), wallet.load(), chain.clone()).unwrap();
    assert!(
        gateway.tick(T0 + 100).await.unwrap(),
        "catch-up is spread over several ticks"
    );
    while gateway.tick(T0 + 100).await.unwrap() {}
    let paid = store.invoice(&invoice.id).unwrap().unwrap();
    assert_eq!((paid.status, paid.confirmations), (Status::Confirmed, 71));
}

#[tokio::test]
async fn node_outage_is_reported_and_recovered_from() {
    let s = setup();
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();

    s.chain.set_down(true);
    assert!(s.gateway.tick(T0 + 1).await.is_err());
    assert!(s.gateway.health().last_error.is_some());

    s.chain.set_down(false);
    s.chain.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 2).await;
    assert!(s.gateway.health().last_error.is_none());
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);
}

#[tokio::test]
async fn switching_to_a_shorter_chain_restarts_scanning() {
    let s = setup();
    for _ in 0..300 {
        s.chain.mine(vec![]);
    }
    s.tick(T0).await;
    let invoice = s.gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();

    // The node list now points at another, much shorter chain.
    let other = Arc::new(MockChain::new(20));
    s.gateway.set_chain(other.clone());
    s.tick(T0 + 1).await;
    other.mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    s.tick(T0 + 2).await;
    assert_eq!(s.invoice(&invoice.id).status, Status::Confirmed);
}

#[tokio::test]
async fn rejects_invalid_invoices() {
    let s = setup();
    assert!(s.gateway.create_invoice(new_invoice(0, 1), T0).is_err());
    assert!(s
        .gateway
        .create_invoice(
            NewInvoice {
                ttl_secs: 5,
                ..new_invoice(XMR, 1)
            },
            T0
        )
        .is_err());
    assert!(s
        .gateway
        .create_invoice(new_invoice(XMR, 1000), T0)
        .is_err());
}

// --- several nodes ---

struct Cluster {
    nodes: Vec<Arc<MockChain>>,
    pool: Arc<NodePool>,
}

fn cluster(count: usize) -> Cluster {
    let nodes: Vec<Arc<MockChain>> = (0..count).map(|_| Arc::new(MockChain::default())).collect();
    let pool = Arc::new(NodePool::new(
        nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (format!("node{i}"), node.clone() as Arc<dyn Chain>))
            .collect(),
    ));
    let cluster = Cluster { nodes, pool };
    for _ in 0..10 {
        cluster.mine_everywhere(vec![]);
    }
    cluster
}

impl Cluster {
    /// Mine a block on the first node and propagate it to the others.
    fn mine_everywhere(&self, txs: Vec<([u8; 32], common::Tx)>) {
        let block = self.nodes[0].mine(txs);
        for node in &self.nodes[1..] {
            node.accept(block.clone());
        }
    }

    fn gateway(&self) -> (Gateway, Arc<Store>) {
        let store = Arc::new(Store::in_memory().unwrap());
        let gateway = Gateway::new(
            store.clone(),
            TestWallet::random().load(),
            self.pool.clone(),
        )
        .unwrap();
        (gateway, store)
    }
}

#[tokio::test]
async fn pool_fails_over_when_a_node_goes_down() {
    let c = cluster(3);
    let (gateway, store) = c.gateway();
    gateway.tick(T0).await.unwrap();
    let invoice = gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();

    c.nodes[0].set_down(true);
    let block = c.nodes[1].mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    c.nodes[2].accept(block);
    gateway.tick(T0 + 1).await.unwrap();
    assert_eq!(
        store.invoice(&invoice.id).unwrap().unwrap().status,
        Status::Confirmed
    );

    let status = c.pool.status();
    assert!(!status[0].healthy && status[1].healthy && status[2].healthy);
}

#[tokio::test]
async fn a_block_served_by_a_single_lying_node_is_never_accepted() {
    let c = cluster(3);
    let (gateway, store) = c.gateway();
    gateway.tick(T0).await.unwrap();
    let invoice = gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();

    // Node 0 (the preferred one) invents a block paying the invoice. The honest nodes mine an
    // empty block at the same height.
    c.nodes[0].mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    let honest = c.nodes[1].mine(vec![]);
    c.nodes[2].accept(honest);

    for i in 1..5 {
        let _ = gateway.tick(T0 + i).await;
    }
    let result = store.invoice(&invoice.id).unwrap().unwrap();
    assert_eq!((result.status, result.received), (Status::Pending, 0));
    assert_eq!(
        gateway.health().scanned,
        Some(c.nodes[1].height()),
        "scanning continued on honest nodes"
    );
    assert!(c
        .pool
        .take_notices()
        .iter()
        .any(|notice| notice.contains("disagree")));
}

#[tokio::test]
async fn two_disagreeing_nodes_stall_instead_of_guessing() {
    let c = cluster(2);
    let (gateway, store) = c.gateway();
    gateway.tick(T0).await.unwrap();
    let invoice = gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();

    c.nodes[0].mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    c.nodes[1].mine(vec![]);
    for i in 1..5 {
        assert!(gateway.tick(T0 + i).await.is_err());
    }
    assert_eq!(
        store.invoice(&invoice.id).unwrap().unwrap().status,
        Status::Pending
    );
}

#[tokio::test]
async fn a_node_claiming_an_absurd_height_is_ignored() {
    let c = cluster(3);
    c.nodes[2].claim_tip(Some(9_999_999));
    assert_eq!(c.pool.tip().await.unwrap(), c.nodes[0].height());

    // A node stuck far behind is marked unhealthy and not used.
    c.nodes[2].claim_tip(Some(1));
    assert_eq!(c.pool.tip().await.unwrap(), c.nodes[0].height());
    assert!(!c.pool.status()[2].healthy);
}

#[tokio::test]
async fn a_node_hiding_a_payment_cannot_stop_detection() {
    let c = cluster(3);
    let (gateway, store) = c.gateway();
    gateway.tick(T0).await.unwrap();
    let invoice = gateway.create_invoice(new_invoice(XMR, 1), T0).unwrap();

    // Node 0 refuses to move past the old tip while the others mine the payment.
    let block = c.nodes[1].mine(vec![(
        common::random_hash(),
        pay_address(&invoice.address, XMR),
    )]);
    c.nodes[2].accept(block);
    gateway.tick(T0 + 1).await.unwrap();
    assert_eq!(
        store.invoice(&invoice.id).unwrap().unwrap().status,
        Status::Confirmed
    );
}
