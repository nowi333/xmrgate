//! Persistent state in a single SQLite file.

use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection, OptionalExtension};

/// How long an expired, unpaid invoice keeps its subaddress before it can be reused.
pub const RELEASE_GRACE_SECS: i64 = 24 * 3600;

#[derive(Debug, thiserror::Error)]
#[error("storage error: {0}")]
pub struct StoreError(#[from] rusqlite::Error);

type Result<T> = std::result::Result<T, StoreError>;

/// The lifecycle of an invoice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Nothing received yet.
    Pending,
    /// Something was received, but less than the amount due.
    Underpaid,
    /// The full amount was detected and is waiting for confirmations.
    Seen,
    /// The full amount has the required confirmations.
    Confirmed,
    /// The invoice expired without receiving anything.
    Expired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Underpaid => "underpaid",
            Status::Seen => "seen",
            Status::Confirmed => "confirmed",
            Status::Expired => "expired",
        }
    }

    fn parse(s: &str) -> Status {
        match s {
            "underpaid" => Status::Underpaid,
            "seen" => Status::Seen,
            "confirmed" => Status::Confirmed,
            "expired" => Status::Expired,
            _ => Status::Pending,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Invoice {
    pub id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub watch_until: i64,
    pub sub_index: u32,
    pub address: String,
    /// Amount due, in piconero.
    pub amount: u64,
    pub fiat_amount: Option<String>,
    pub fiat_currency: Option<String>,
    pub order_ref: Option<String>,
    pub description: Option<String>,
    pub callback_url: Option<String>,
    pub redirect_url: Option<String>,
    pub confirmations_required: u32,
    pub status: Status,
    /// Total received so far, confirmed or not, in piconero.
    pub received: u64,
    /// Confirmations of the least-confirmed payment, 0 while any is unconfirmed.
    pub confirmations: u32,
    /// The site that created the invoice through the API, if any.
    pub site_id: Option<i64>,
    /// The payment button the invoice came from, if any.
    pub button_id: Option<String>,
}

/// A website allowed to create invoices, with its own secret key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Site {
    pub id: i64,
    pub name: String,
    pub api_key: String,
    pub created_at: i64,
}

/// A reusable payment link with a fixed price, for sites without any server code.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Button {
    pub id: String,
    pub name: String,
    /// The price as entered, in `currency`.
    pub amount: String,
    pub currency: String,
    pub redirect_url: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoredPayment {
    pub output_key: [u8; 32],
    pub invoice_id: String,
    pub txid: [u8; 32],
    pub amount: u64,
    pub height: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Webhook {
    pub id: i64,
    pub site_id: Option<i64>,
    pub url: String,
    pub payload: String,
    pub attempts: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Event {
    pub at: i64,
    pub kind: String,
    pub message: String,
}

pub struct Store {
    db: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS invoices (
  id TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  watch_until INTEGER NOT NULL,
  sub_index INTEGER NOT NULL,
  address TEXT NOT NULL,
  amount INTEGER NOT NULL,
  fiat_amount TEXT, fiat_currency TEXT, order_ref TEXT, description TEXT,
  callback_url TEXT, redirect_url TEXT,
  confirmations_required INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  received INTEGER NOT NULL DEFAULT 0,
  confirmations INTEGER NOT NULL DEFAULT 0,
  released INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS invoices_active ON invoices (released, sub_index);
CREATE INDEX IF NOT EXISTS invoices_created ON invoices (created_at);
CREATE TABLE IF NOT EXISTS payments (
  output_key BLOB PRIMARY KEY,
  invoice_id TEXT NOT NULL,
  txid BLOB NOT NULL,
  amount INTEGER NOT NULL,
  height INTEGER,
  seen_at INTEGER NOT NULL,
  missing_since INTEGER
);
CREATE INDEX IF NOT EXISTS payments_invoice ON payments (invoice_id);
CREATE TABLE IF NOT EXISTS blocks (height INTEGER PRIMARY KEY, hash BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS webhooks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  invoice_id TEXT NOT NULL,
  url TEXT NOT NULL,
  payload TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  delivered_at INTEGER,
  last_error TEXT
);
CREATE TABLE IF NOT EXISTS sites (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  api_key TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS buttons (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  amount TEXT NOT NULL,
  currency TEXT NOT NULL,
  redirect_url TEXT,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, kind TEXT NOT NULL, message TEXT NOT NULL
);
";

const INVOICE_COLUMNS: &str = "id, created_at, expires_at, watch_until, sub_index, address, amount,
  fiat_amount, fiat_currency, order_ref, description, callback_url, redirect_url,
  confirmations_required, status, received, confirmations, site_id, button_id";

fn invoice_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Invoice> {
    Ok(Invoice {
        id: row.get(0)?,
        created_at: row.get(1)?,
        expires_at: row.get(2)?,
        watch_until: row.get(3)?,
        sub_index: row.get(4)?,
        address: row.get(5)?,
        amount: row.get::<_, i64>(6)? as u64,
        fiat_amount: row.get(7)?,
        fiat_currency: row.get(8)?,
        order_ref: row.get(9)?,
        description: row.get(10)?,
        callback_url: row.get(11)?,
        redirect_url: row.get(12)?,
        confirmations_required: row.get(13)?,
        status: Status::parse(&row.get::<_, String>(14)?),
        received: row.get::<_, i64>(15)? as u64,
        confirmations: row.get(16)?,
        site_id: row.get(17)?,
        button_id: row.get(18)?,
    })
}

impl Store {
    /// Open (or create) the database at `path`. The file is made private to its owner, since it
    /// holds the view key.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(db)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.execute_batch(SCHEMA)?;
        // Columns added after the first release.
        for (table, column, declaration) in [
            ("invoices", "site_id", "INTEGER"),
            ("invoices", "button_id", "TEXT"),
            ("webhooks", "site_id", "INTEGER"),
        ] {
            let present = db
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
                ))?
                .exists([column])?;
            if !present {
                db.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
                ))?;
            }
        }
        Ok(Self { db: Mutex::new(db) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // --- configuration ---

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db()
            .query_row("SELECT value FROM config WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db().execute(
            "INSERT INTO config (key, value) VALUES (?1, ?2)
       ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    // --- invoices ---

    /// Insert an invoice, assigning it the lowest subaddress index not held by another invoice.
    ///
    /// Indexes of invoices that expired unpaid are reused after a grace period. This keeps indexes
    /// low, so the merchant's own wallet finds every payment within its default lookahead.
    /// `address_for` derives the address for the chosen index.
    pub fn insert_invoice(
        &self,
        mut invoice: Invoice,
        address_for: impl Fn(u32) -> String,
    ) -> Result<Invoice> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let index = {
            let mut stmt =
                tx.prepare("SELECT sub_index FROM invoices WHERE released = 0 ORDER BY sub_index")?;
            let mut next = 1u32;
            for used in stmt.query_map([], |row| row.get::<_, u32>(0))? {
                let used = used?;
                if used > next {
                    break;
                }
                if used == next {
                    next += 1;
                }
            }
            next
        };
        invoice.sub_index = index;
        invoice.address = address_for(index);
        tx.execute(
      &format!(
        "INSERT INTO invoices ({INVOICE_COLUMNS})
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"
      ),
      params![
        invoice.id,
        invoice.created_at,
        invoice.expires_at,
        invoice.watch_until,
        invoice.sub_index,
        invoice.address,
        invoice.amount as i64,
        invoice.fiat_amount,
        invoice.fiat_currency,
        invoice.order_ref,
        invoice.description,
        invoice.callback_url,
        invoice.redirect_url,
        invoice.confirmations_required,
        invoice.status.as_str(),
        invoice.received as i64,
        invoice.confirmations,
        invoice.site_id,
        invoice.button_id,
      ],
    )?;
        tx.commit()?;
        Ok(invoice)
    }

    pub fn invoice(&self, id: &str) -> Result<Option<Invoice>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {INVOICE_COLUMNS} FROM invoices WHERE id = ?1"),
                [id],
                invoice_from_row,
            )
            .optional()?)
    }

    fn invoices_where(&self, clause: &str, args: impl rusqlite::Params) -> Result<Vec<Invoice>> {
        let db = self.db();
        let mut stmt = db.prepare(&format!("SELECT {INVOICE_COLUMNS} FROM invoices {clause}"))?;
        let rows = stmt.query_map(args, invoice_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Most recent invoices first.
    pub fn recent_invoices(&self, limit: u32) -> Result<Vec<Invoice>> {
        self.invoices_where("ORDER BY created_at DESC, rowid DESC LIMIT ?1", [limit])
    }

    /// Invoices still holding their subaddress: these must be registered with the scanner.
    pub fn unreleased_invoices(&self) -> Result<Vec<Invoice>> {
        self.invoices_where("WHERE released = 0", [])
    }

    /// Invoices whose payments or status may still change.
    pub fn active_invoices(&self, now: i64) -> Result<Vec<Invoice>> {
        self.invoices_where(
            "WHERE released = 0 AND (watch_until > ?1 OR status IN ('pending', 'seen'))",
            [now],
        )
    }

    /// The unreleased invoice holding a subaddress index, if any.
    pub fn invoice_by_index(&self, index: u32) -> Result<Option<Invoice>> {
        Ok(self
            .invoices_where("WHERE released = 0 AND sub_index = ?1", [index])?
            .pop())
    }

    pub fn update_invoice_state(
        &self,
        id: &str,
        status: Status,
        received: u64,
        confirmations: u32,
        watch_until: i64,
    ) -> Result<()> {
        self.db().execute(
            "UPDATE invoices SET status = ?2, received = ?3, confirmations = ?4, watch_until = ?5
       WHERE id = ?1",
            params![
                id,
                status.as_str(),
                received as i64,
                confirmations,
                watch_until
            ],
        )?;
        Ok(())
    }

    /// Free the subaddresses of invoices that expired unpaid long enough ago.
    pub fn release_expired(&self, now: i64) -> Result<usize> {
        Ok(self.db().execute(
            "UPDATE invoices SET released = 1
       WHERE released = 0 AND status = 'expired' AND expires_at + ?2 < ?1
         AND NOT EXISTS (SELECT 1 FROM payments WHERE invoice_id = invoices.id)",
            params![now, RELEASE_GRACE_SECS],
        )?)
    }

    /// `(invoices, confirmed invoices, piconero received on confirmed invoices)`.
    pub fn totals(&self) -> Result<(u64, u64, u64)> {
        Ok(self.db().query_row(
            "SELECT COUNT(*), COALESCE(SUM(status = 'confirmed'), 0),
              COALESCE(SUM(CASE WHEN status = 'confirmed' THEN received ELSE 0 END), 0)
       FROM invoices",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            },
        )?)
    }

    // --- sites ---

    pub fn insert_site(&self, name: &str, api_key: &str, now: i64) -> Result<Site> {
        let db = self.db();
        db.execute(
            "INSERT INTO sites (name, api_key, created_at) VALUES (?1, ?2, ?3)",
            params![name, api_key, now],
        )?;
        Ok(Site {
            id: db.last_insert_rowid(),
            name: name.to_owned(),
            api_key: api_key.to_owned(),
            created_at: now,
        })
    }

    fn sites_where(&self, clause: &str, args: impl rusqlite::Params) -> Result<Vec<Site>> {
        let db = self.db();
        let mut stmt = db.prepare(&format!(
            "SELECT id, name, api_key, created_at FROM sites {clause}"
        ))?;
        let rows = stmt.query_map(args, |row| {
            Ok(Site {
                id: row.get(0)?,
                name: row.get(1)?,
                api_key: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn sites(&self) -> Result<Vec<Site>> {
        self.sites_where("ORDER BY id", [])
    }

    pub fn site(&self, id: i64) -> Result<Option<Site>> {
        Ok(self.sites_where("WHERE id = ?1", [id])?.pop())
    }

    pub fn set_site_key(&self, id: i64, api_key: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE sites SET api_key = ?2 WHERE id = ?1",
            params![id, api_key],
        )? > 0)
    }

    pub fn delete_site(&self, id: i64) -> Result<bool> {
        Ok(self.db().execute("DELETE FROM sites WHERE id = ?1", [id])? > 0)
    }

    // --- payment buttons ---

    pub fn insert_button(&self, button: &Button) -> Result<()> {
        self.db().execute(
            "INSERT INTO buttons (id, name, amount, currency, redirect_url, created_at)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                button.id,
                button.name,
                button.amount,
                button.currency,
                button.redirect_url,
                button.created_at
            ],
        )?;
        Ok(())
    }

    fn buttons_where(&self, clause: &str, args: impl rusqlite::Params) -> Result<Vec<Button>> {
        let db = self.db();
        let mut stmt = db.prepare(&format!(
            "SELECT id, name, amount, currency, redirect_url, created_at FROM buttons {clause}"
        ))?;
        let rows = stmt.query_map(args, |row| {
            Ok(Button {
                id: row.get(0)?,
                name: row.get(1)?,
                amount: row.get(2)?,
                currency: row.get(3)?,
                redirect_url: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn buttons(&self) -> Result<Vec<Button>> {
        self.buttons_where("ORDER BY created_at DESC, rowid DESC", [])
    }

    pub fn button(&self, id: &str) -> Result<Option<Button>> {
        Ok(self.buttons_where("WHERE id = ?1", [id])?.pop())
    }

    pub fn delete_button(&self, id: &str) -> Result<bool> {
        Ok(self
            .db()
            .execute("DELETE FROM buttons WHERE id = ?1", [id])?
            > 0)
    }

    /// How many invoices from a button are still waiting for a first payment.
    pub fn open_invoices_for_button(&self, id: &str, now: i64) -> Result<u32> {
        Ok(self.db().query_row(
            "SELECT COUNT(*) FROM invoices
       WHERE button_id = ?1 AND status = 'pending' AND expires_at > ?2",
            params![id, now],
            |row| row.get(0),
        )?)
    }

    // --- payments ---

    /// Record a payment. Returns false if this output key was already recorded.
    ///
    /// A known key seen again at a height confirms a mempool payment. A known key in a different
    /// transaction is a duplicate output that could never be spent, and is ignored.
    pub fn record_payment(&self, payment: &StoredPayment, now: i64) -> Result<bool> {
        let db = self.db();
        let existing: Option<Vec<u8>> = db
            .query_row(
                "SELECT txid FROM payments WHERE output_key = ?1",
                [payment.output_key.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            None => {
                db.execute(
                    "INSERT INTO payments (output_key, invoice_id, txid, amount, height, seen_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        payment.output_key.as_slice(),
                        payment.invoice_id,
                        payment.txid.as_slice(),
                        payment.amount as i64,
                        payment.height.map(|h| h as i64),
                        now
                    ],
                )?;
                Ok(true)
            }
            Some(txid) if txid == payment.txid => {
                if payment.height.is_some() {
                    db.execute(
            "UPDATE payments SET height = ?2, missing_since = NULL WHERE output_key = ?1",
            params![payment.output_key.as_slice(), payment.height.map(|h| h as i64)],
          )?;
                }
                Ok(false)
            }
            Some(_) => Ok(false),
        }
    }

    pub fn payments_for(&self, invoice_id: &str) -> Result<Vec<StoredPayment>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT output_key, invoice_id, txid, amount, height FROM payments
       WHERE invoice_id = ?1 ORDER BY seen_at, rowid",
        )?;
        let rows = stmt.query_map([invoice_id], |row| {
            let key: Vec<u8> = row.get(0)?;
            let txid: Vec<u8> = row.get(2)?;
            Ok(StoredPayment {
                output_key: key.try_into().unwrap_or([0; 32]),
                invoice_id: row.get(1)?,
                txid: txid.try_into().unwrap_or([0; 32]),
                amount: row.get::<_, i64>(3)? as u64,
                height: row.get::<_, Option<i64>>(4)?.map(|h| h as u64),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Transaction hashes of payments not yet in a block.
    pub fn unconfirmed_txids(&self) -> Result<Vec<[u8; 32]>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT DISTINCT txid FROM payments WHERE height IS NULL")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = vec![];
        for txid in rows {
            if let Ok(txid) = <[u8; 32]>::try_from(txid?) {
                out.push(txid);
            }
        }
        Ok(out)
    }

    /// Note whether an unconfirmed transaction is still in the mempool, and drop its payments if
    /// it has been gone for longer than `drop_after` seconds. Returns true if payments were dropped.
    pub fn track_unconfirmed(
        &self,
        txid: &[u8; 32],
        in_mempool: bool,
        now: i64,
        drop_after: i64,
    ) -> Result<bool> {
        let db = self.db();
        if in_mempool {
            db.execute(
                "UPDATE payments SET missing_since = NULL WHERE txid = ?1 AND height IS NULL",
                [txid.as_slice()],
            )?;
            return Ok(false);
        }
        db.execute(
            "UPDATE payments SET missing_since = ?2
       WHERE txid = ?1 AND height IS NULL AND missing_since IS NULL",
            params![txid.as_slice(), now],
        )?;
        let dropped = db.execute(
            "DELETE FROM payments WHERE txid = ?1 AND height IS NULL AND missing_since + ?3 <= ?2",
            params![txid.as_slice(), now, drop_after],
        )?;
        Ok(dropped > 0)
    }

    // --- scanned chain ---

    /// The highest scanned block as `(height, hash)`.
    pub fn last_block(&self) -> Result<Option<(u64, [u8; 32])>> {
        Ok(self
            .db()
            .query_row(
                "SELECT height, hash FROM blocks ORDER BY height DESC LIMIT 1",
                [],
                |row| {
                    let hash: Vec<u8> = row.get(1)?;
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        hash.try_into().unwrap_or([0; 32]),
                    ))
                },
            )
            .optional()?)
    }

    pub fn put_block(&self, height: u64, hash: &[u8; 32]) -> Result<()> {
        let db = self.db();
        db.execute(
            "INSERT OR REPLACE INTO blocks (height, hash) VALUES (?1, ?2)",
            params![height as i64, hash.as_slice()],
        )?;
        db.execute(
            "DELETE FROM blocks WHERE height < ?1",
            [height as i64 - 200],
        )?;
        Ok(())
    }

    /// Forget every block at or above `height`. Payments they held go back to unconfirmed, and are
    /// dropped later unless the transaction is mined again or returns to the mempool.
    pub fn rollback(&self, height: u64, now: i64) -> Result<()> {
        let db = self.db();
        db.execute("DELETE FROM blocks WHERE height >= ?1", [height as i64])?;
        db.execute(
            "UPDATE payments SET height = NULL, missing_since = ?2 WHERE height >= ?1",
            params![height as i64, now],
        )?;
        Ok(())
    }

    /// Forget all scanned blocks, used to jump ahead when nothing needs watching.
    pub fn reset_blocks(&self, height: u64, hash: &[u8; 32]) -> Result<()> {
        let db = self.db();
        db.execute("DELETE FROM blocks", [])?;
        db.execute(
            "INSERT INTO blocks (height, hash) VALUES (?1, ?2)",
            params![height as i64, hash.as_slice()],
        )?;
        Ok(())
    }

    // --- webhooks ---

    pub fn enqueue_webhook(
        &self,
        invoice_id: &str,
        site_id: Option<i64>,
        url: &str,
        payload: &str,
        now: i64,
    ) -> Result<()> {
        self.db().execute(
            "INSERT INTO webhooks (invoice_id, site_id, url, payload, next_attempt_at)
       VALUES (?1, ?2, ?3, ?4, ?5)",
            params![invoice_id, site_id, url, payload, now],
        )?;
        Ok(())
    }

    pub fn due_webhooks(&self, now: i64) -> Result<Vec<Webhook>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id, site_id, url, payload, attempts FROM webhooks
       WHERE delivered_at IS NULL AND next_attempt_at <= ?1 ORDER BY id LIMIT 20",
        )?;
        let rows = stmt.query_map([now], |row| {
            Ok(Webhook {
                id: row.get(0)?,
                site_id: row.get(1)?,
                url: row.get(2)?,
                payload: row.get(3)?,
                attempts: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn webhook_delivered(&self, id: i64, now: i64) -> Result<()> {
        self.db().execute(
            "UPDATE webhooks SET delivered_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    /// Schedule a retry, or give up by pushing the next attempt out of reach after 12 failures.
    pub fn webhook_failed(&self, id: i64, error: &str, next_attempt_at: Option<i64>) -> Result<()> {
        self.db().execute(
            "UPDATE webhooks SET attempts = attempts + 1, last_error = ?2, next_attempt_at = ?3
       WHERE id = ?1",
            params![id, error, next_attempt_at.unwrap_or(i64::MAX)],
        )?;
        Ok(())
    }

    // --- events ---

    pub fn log(&self, kind: &str, message: &str, now: i64) -> Result<()> {
        let db = self.db();
        db.execute(
            "INSERT INTO events (at, kind, message) VALUES (?1, ?2, ?3)",
            params![now, kind, message],
        )?;
        db.execute(
            "DELETE FROM events WHERE id <= (SELECT MAX(id) FROM events) - 500",
            [],
        )?;
        Ok(())
    }

    pub fn recent_events(&self, limit: u32) -> Result<Vec<Event>> {
        let db = self.db();
        let mut stmt =
            db.prepare("SELECT at, kind, message FROM events ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit], |row| {
            Ok(Event {
                at: row.get(0)?,
                kind: row.get(1)?,
                message: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
