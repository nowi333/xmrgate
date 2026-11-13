//! The payment engine: invoices, the scan loop, and webhook delivery.

use std::{
  collections::HashSet,
  sync::{Arc, Mutex, RwLock},
};

use hmac::{Hmac, KeyInit, Mac};
use rand::RngCore;
use sha2::Sha256;

use crate::{
  amount::format_xmr,
  chain::{Chain, ChainError},
  scan::{Payment, PaymentScanner, ScanError},
  store::{Invoice, Status, Store, StoreError, StoredPayment, RELEASE_GRACE_SECS},
  wallet::ViewWallet,
};

/// Blocks scanned per tick at most, so a long catch-up cannot starve everything else.
const BLOCKS_PER_TICK: u64 = 30;
/// How deep a reorganisation is followed before giving up and resyncing from the tip.
const MAX_REORG_DEPTH: u64 = 100;
/// An unconfirmed payment missing from the mempool for this long is considered dropped.
const MEMPOOL_DROP_SECS: i64 = 600;
/// A confirmed invoice stays watched this long, so a reorganisation can still demote it.
const POST_CONFIRM_WATCH_SECS: i64 = 2 * 3600;
const WEBHOOK_MAX_ATTEMPTS: u32 = 12;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
  #[error("node error: {0}")]
  Chain(#[from] ChainError),
  #[error(transparent)]
  Store(#[from] StoreError),
  #[error(transparent)]
  Scan(#[from] ScanError),
  #[error("invalid invoice: {0}")]
  InvalidInvoice(&'static str),
}

/// Parameters for a new invoice.
#[derive(Clone, Debug, Default)]
pub struct NewInvoice {
  /// Amount due in piconero.
  pub amount: u64,
  pub fiat_amount: Option<String>,
  pub fiat_currency: Option<String>,
  pub order_ref: Option<String>,
  pub description: Option<String>,
  pub callback_url: Option<String>,
  pub redirect_url: Option<String>,
  pub confirmations: u32,
  pub ttl_secs: i64,
  pub site_id: Option<i64>,
  pub button_id: Option<String>,
}

/// Scanner progress, for the health screen.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Health {
  /// Height agreed on by the nodes.
  pub tip: Option<u64>,
  /// Highest block processed.
  pub scanned: Option<u64>,
  pub last_ok_at: Option<i64>,
  pub last_error: Option<String>,
  /// True when no invoice needs watching and no blocks are being downloaded.
  pub idle: bool,
}

pub struct Gateway {
  store: Arc<Store>,
  wallet: ViewWallet,
  chain: RwLock<Arc<dyn Chain>>,
  scanner: Mutex<PaymentScanner>,
  mempool_seen: Mutex<HashSet<[u8; 32]>>,
  health: Mutex<Health>,
}

impl Gateway {
  pub fn new(store: Arc<Store>, wallet: ViewWallet, chain: Arc<dyn Chain>) -> Result<Self, GatewayError> {
    let mut scanner = PaymentScanner::new(&wallet);
    for invoice in store.unreleased_invoices()? {
      let _ = scanner.watch_subaddress(0, invoice.sub_index);
    }
    Ok(Self {
      store,
      wallet,
      chain: RwLock::new(chain),
      scanner: Mutex::new(scanner),
      mempool_seen: Mutex::new(HashSet::new()),
      health: Mutex::new(Health::default()),
    })
  }

  pub fn store(&self) -> &Arc<Store> {
    &self.store
  }

  pub fn wallet(&self) -> &ViewWallet {
    &self.wallet
  }

  pub fn health(&self) -> Health {
    self.health.lock().unwrap().clone()
  }

  /// Replace the node source, after the node list was edited.
  pub fn set_chain(&self, chain: Arc<dyn Chain>) {
    *self.chain.write().unwrap() = chain;
  }

  fn chain(&self) -> Arc<dyn Chain> {
    self.chain.read().unwrap().clone()
  }

  /// Create an invoice with its own subaddress.
  pub fn create_invoice(&self, new: NewInvoice, now: i64) -> Result<Invoice, GatewayError> {
    if new.amount == 0 {
      return Err(GatewayError::InvalidInvoice("amount must be greater than zero"));
    }
    if new.amount > i64::MAX as u64 {
      return Err(GatewayError::InvalidInvoice("amount is too large"));
    }
    if !(60 ..= 7 * 24 * 3600).contains(&new.ttl_secs) {
      return Err(GatewayError::InvalidInvoice("expiry must be between 1 minute and 7 days"));
    }
    if new.confirmations > 100 {
      return Err(GatewayError::InvalidInvoice("at most 100 confirmations can be required"));
    }

    let mut id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut id);
    let expires_at = now + new.ttl_secs;
    let invoice = Invoice {
      id: hex::encode(id),
      created_at: now,
      expires_at,
      watch_until: expires_at + RELEASE_GRACE_SECS,
      sub_index: 0,
      address: String::new(),
      amount: new.amount,
      fiat_amount: new.fiat_amount,
      fiat_currency: new.fiat_currency,
      order_ref: new.order_ref,
      description: new.description,
      callback_url: new.callback_url,
      redirect_url: new.redirect_url,
      confirmations_required: new.confirmations,
      status: Status::Pending,
      received: 0,
      confirmations: 0,
      site_id: new.site_id,
      button_id: new.button_id,
    };
    let invoice = self.store.insert_invoice(invoice, |index| {
      self.wallet.subaddress(0, index).expect("index is never zero")
    })?;
    self
      .scanner
      .lock()
      .unwrap()
      .watch_subaddress(0, invoice.sub_index)
      .expect("index is never zero");
    Ok(invoice)
  }

  /// Run one iteration of the scan loop. Returns true if more blocks are waiting, in which case
  /// the caller should tick again right away.
  pub async fn tick(&self, now: i64) -> Result<bool, GatewayError> {
    let result = self.tick_inner(now).await;
    let mut health = self.health.lock().unwrap();
    match &result {
      Ok(_) => {
        health.last_ok_at = Some(now);
        health.last_error = None;
      }
      Err(e) => health.last_error = Some(e.to_string()),
    }
    result
  }

  async fn tick_inner(&self, now: i64) -> Result<bool, GatewayError> {
    let chain = self.chain();
    // The tip is read before checking for active invoices. An invoice created after this point
    // cannot have been paid in a block at or below this tip, so skipping those blocks is safe.
    let tip = chain.tip().await?;
    self.health.lock().unwrap().tip = Some(tip);

    self.store.release_expired(now)?;
    let active = self.store.active_invoices(now)?;
    let last = self.store.last_block()?;

    if let Some((height, _)) = last {
      if height > tip + MAX_REORG_DEPTH {
        // The nodes are far behind what was scanned: they follow another chain (the node list
        // was changed to another network, or the data was restored elsewhere). Start over.
        let hash = chain.block_hash(tip).await?;
        self.store.reset_blocks(tip, &hash)?;
        self.mempool_seen.lock().unwrap().clear();
        self.store.log("resync", &format!("the nodes are on a different chain; restarting from block {tip}"), now)?;
        return Ok(true);
      }
    }

    if active.is_empty() || last.is_none() {
      // Nothing to look for: remember where the chain is and download nothing.
      if last.map(|(height, _)| height) != Some(tip) {
        let hash = chain.block_hash(tip).await?;
        self.store.reset_blocks(tip, &hash)?;
      }
      self.mempool_seen.lock().unwrap().clear();
      let mut health = self.health.lock().unwrap();
      health.scanned = Some(tip);
      health.idle = active.is_empty();
      return Ok(false);
    }
    self.health.lock().unwrap().idle = false;

    let (mut height, mut hash) = last.expect("checked above");
    let mut more = false;

    if height <= tip {
      // Make sure the last block we scanned is still part of the chain.
      let mut depth = 0;
      while chain.block_hash(height).await? != hash {
        self.store.rollback(height, now)?;
        self.store.log("reorg", &format!("block {height} was replaced"), now)?;
        depth += 1;
        match self.store.last_block()? {
          Some(previous) if depth < MAX_REORG_DEPTH => (height, hash) = previous,
          _ => {
            // Deeper than anything we kept: restart from the tip on the next tick.
            self.store.rollback(0, now)?;
            self.store.log("reorg", "chain history changed beyond what is kept; resyncing", now)?;
            return Ok(true);
          }
        }
      }

      let target = tip.min(height + BLOCKS_PER_TICK);
      more = target < tip;
      while height < target {
        let block = chain.block(height + 1).await?;
        if block.block.header.previous != hash {
          // The chain moved between two requests; sort it out on the next tick.
          return Ok(true);
        }
        let block_hash = block.block.hash();
        let payments = self.scanner.lock().unwrap().scan_block(block)?;
        height += 1;
        hash = block_hash;
        self.record(payments, Some(height), now)?;
        self.store.put_block(height, &hash)?;
      }
    }
    self.health.lock().unwrap().scanned = Some(height);

    if !more && height >= tip {
      self.scan_mempool(&*chain, now).await?;
    }

    self.evaluate(height, now)?;
    Ok(more)
  }

  async fn scan_mempool(&self, chain: &dyn Chain, now: i64) -> Result<(), GatewayError> {
    let hashes = chain.mempool_hashes().await?;
    let current: HashSet<[u8; 32]> = hashes.iter().copied().collect();

    let fresh: Vec<[u8; 32]> = {
      let seen = self.mempool_seen.lock().unwrap();
      hashes.iter().filter(|hash| !seen.contains(*hash)).copied().collect()
    };
    // A transaction can be mined between the two requests, which makes the fetch fail. That is
    // harmless: it is found in its block on the next tick.
    if let Ok(transactions) = chain.transactions(&fresh).await {
      let payments = self
        .scanner
        .lock()
        .unwrap()
        .scan_mempool(fresh.iter().copied().zip(transactions).collect())?;
      self.record(payments, None, now)?;
      *self.mempool_seen.lock().unwrap() = current.clone();
    }

    for txid in self.store.unconfirmed_txids()? {
      let present = current.contains(&txid);
      if self.store.track_unconfirmed(&txid, present, now, MEMPOOL_DROP_SECS)? {
        self.store.log(
          "payment_dropped",
          &format!("transaction {} left the mempool without being mined", hex::encode(txid)),
          now,
        )?;
      }
    }
    Ok(())
  }

  fn record(&self, payments: Vec<Payment>, height: Option<u64>, now: i64) -> Result<(), GatewayError> {
    for payment in payments {
      let Some((0, index)) = payment.subaddress else { continue };
      let Some(invoice) = self.store.invoice_by_index(index)? else {
        self.store.log(
          "unmatched_payment",
          &format!(
            "received {} XMR on subaddress #{index}, which belongs to no open invoice (tx {})",
            format_xmr(payment.amount),
            hex::encode(payment.txid)
          ),
          now,
        )?;
        continue;
      };
      self.store.record_payment(
        &StoredPayment {
          output_key: payment.output_key,
          invoice_id: invoice.id,
          txid: payment.txid,
          amount: payment.amount,
          height,
        },
        now,
      )?;
    }
    Ok(())
  }

  /// Recompute the status of every active invoice from its payments.
  fn evaluate(&self, scanned: u64, now: i64) -> Result<(), GatewayError> {
    for invoice in self.store.active_invoices(now)? {
      let mut payments: Vec<(u64, u32)> = self
        .store
        .payments_for(&invoice.id)?
        .into_iter()
        .map(|payment| {
          let confirmations = payment
            .height
            .map(|height| scanned.saturating_sub(height).saturating_add(1).min(u32::MAX as u64) as u32)
            .unwrap_or(0);
          (payment.amount, confirmations)
        })
        .collect();
      let received = payments.iter().fold(0u64, |sum, (amount, _)| sum.saturating_add(*amount));

      // The invoice is as confirmed as the least-confirmed payment needed to reach the amount,
      // counting the best-confirmed payments first.
      payments.sort_by_key(|payment| std::cmp::Reverse(payment.1));
      let mut covered = 0u64;
      let mut confirmations = 0;
      for (amount, payment_confirmations) in &payments {
        covered = covered.saturating_add(*amount);
        confirmations = *payment_confirmations;
        if covered >= invoice.amount {
          break;
        }
      }

      let status = if received >= invoice.amount {
        if confirmations >= invoice.confirmations_required {
          Status::Confirmed
        } else {
          Status::Seen
        }
      } else if received > 0 {
        confirmations = 0;
        Status::Underpaid
      } else if now >= invoice.expires_at {
        Status::Expired
      } else {
        Status::Pending
      };

      let mut watch_until = invoice.watch_until;
      match status {
        Status::Seen => watch_until = watch_until.max(now + RELEASE_GRACE_SECS),
        Status::Confirmed if invoice.status != Status::Confirmed => {
          watch_until = watch_until.max(now + POST_CONFIRM_WATCH_SECS);
        }
        _ => {}
      }

      if (status, received, confirmations, watch_until) ==
        (invoice.status, invoice.received, invoice.confirmations, invoice.watch_until)
      {
        continue;
      }
      self.store.update_invoice_state(&invoice.id, status, received, confirmations, watch_until)?;

      if status != invoice.status {
        let updated = Invoice { status, received, confirmations, watch_until, ..invoice.clone() };
        self.store.log(
          "invoice",
          &format!(
            "invoice {} ({}) is now {}",
            &invoice.id[.. 8],
            invoice.order_ref.as_deref().unwrap_or("no reference"),
            status.as_str()
          ),
          now,
        )?;
        if let Some(url) = &invoice.callback_url {
          let payload = serde_json::json!({ "event": "invoice.updated", "invoice": invoice_json(&updated) });
          self.store.enqueue_webhook(&invoice.id, invoice.site_id, url, &payload.to_string(), now)?;
        }
      }
    }
    Ok(())
  }

  /// Send pending webhooks. Each is signed with HMAC-SHA256 over the exact body, keyed with the
  /// secret key of the site that created the invoice, hex-encoded in `X-Xmrgate-Signature`.
  pub async fn deliver_webhooks(
    &self,
    client: &reqwest::Client,
    secret_for: impl Fn(Option<i64>) -> Option<String>,
    now: i64,
  ) -> Result<(), GatewayError> {
    for webhook in self.store.due_webhooks(now)? {
      let Some(secret) = secret_for(webhook.site_id) else {
        // The site was removed: nobody is left to notify.
        self.store.webhook_failed(webhook.id, "the site no longer exists", None)?;
        continue;
      };
      let signature = sign(&secret, webhook.payload.as_bytes());
      let outcome = client
        .post(&webhook.url)
        .header("Content-Type", "application/json")
        .header("X-Xmrgate-Signature", signature)
        .body(webhook.payload.clone())
        .send()
        .await
        .map_err(|e| e.without_url().to_string())
        .and_then(|response| {
          if response.status().is_success() {
            Ok(())
          } else {
            Err(format!("the shop answered HTTP {}", response.status().as_u16()))
          }
        });
      match outcome {
        Ok(()) => self.store.webhook_delivered(webhook.id, now)?,
        Err(error) => {
          let attempts = webhook.attempts + 1;
          let retry = (attempts < WEBHOOK_MAX_ATTEMPTS)
            .then(|| now + (60i64 << attempts.min(9)).min(6 * 3600));
          self.store.webhook_failed(webhook.id, &error, retry)?;
          if retry.is_none() {
            self.store.log("webhook", &format!("gave up notifying {}: {error}", webhook.url), now)?;
          }
        }
      }
    }
    Ok(())
  }
}

/// Hex-encoded HMAC-SHA256 of `body` under `secret`.
pub fn sign(secret: &str, body: &[u8]) -> String {
  let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
  mac.update(body);
  hex::encode(mac.finalize().into_bytes())
}

/// The public JSON view of an invoice, used by the API, the checkout page and webhooks.
pub fn invoice_json(invoice: &Invoice) -> serde_json::Value {
  let due = invoice.amount.saturating_sub(invoice.received);
  serde_json::json!({
    "id": invoice.id,
    "status": invoice.status.as_str(),
    "address": invoice.address,
    "amount_xmr": format_xmr(invoice.amount),
    "amount_piconero": invoice.amount,
    "received_xmr": format_xmr(invoice.received),
    "received_piconero": invoice.received,
    "due_xmr": format_xmr(due),
    "payment_uri": format!("monero:{}?tx_amount={}", invoice.address, format_xmr(if due > 0 { due } else { invoice.amount })),
    "fiat_amount": invoice.fiat_amount,
    "fiat_currency": invoice.fiat_currency,
    "order_ref": invoice.order_ref,
    "description": invoice.description,
    "confirmations": invoice.confirmations,
    "confirmations_required": invoice.confirmations_required,
    "created_at": invoice.created_at,
    "expires_at": invoice.expires_at,
    "redirect_url": invoice.redirect_url,
  })
}
