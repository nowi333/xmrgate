//! End-to-end tests of the HTTP server: setup, shop API, checkout and webhooks.

mod common;

use std::sync::{Arc, Mutex};

use axum::{http::HeaderMap, routing::post, Router};
use common::{pay_address, MockChain, TestWallet};
use serde_json::{json, Value};
use xmrgate::{rates::Rates, server, sign, Store};

struct Harness {
  base: String,
  chain: Arc<MockChain>,
  app: Arc<server::App>,
  wallet: TestWallet,
  http: reqwest::Client,
}

async fn serve(router: Router) -> String {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
  format!("http://{addr}")
}

async fn harness() -> Harness {
  let chain = Arc::new(MockChain::new(10));
  let store = Arc::new(Store::in_memory().unwrap());
  let app = server::App::new(store, Rates::fixed("500"), Some("letmein".into()), Some(chain.clone())).unwrap();
  let base = serve(server::router(app.clone())).await;
  let http = reqwest::Client::builder()
    .no_proxy()
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .unwrap();
  Harness { base, chain, app, wallet: TestWallet::random(), http }
}

impl Harness {
  async fn post(&self, path: &str, body: Value, auth: &[(&str, &str)]) -> (u16, Value, HeaderMap) {
    let mut request = self.http.post(format!("{}{path}", self.base)).json(&body);
    for (name, value) in auth {
      request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    let (status, headers) = (response.status().as_u16(), response.headers().clone());
    (status, response.json().await.unwrap_or(Value::Null), headers)
  }

  async fn get(&self, path: &str, auth: &[(&str, &str)]) -> (u16, Value) {
    let mut request = self.http.get(format!("{}{path}", self.base));
    for (name, value) in auth {
      request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap_or(Value::Null))
  }

  /// Complete the setup and return `(session cookie, api key)`.
  async fn set_up(&self) -> (String, String) {
    let (status, _, headers) = self
      .post(
        "/setup/api",
        json!({ "token": "letmein", "address": self.wallet.address, "view_key": self.wallet.view_hex,
                "password": "correct horse", "shop_name": "Test Shop" }),
        &[],
      )
      .await;
    assert_eq!(status, 200);
    let cookie = headers["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned();
    let (_, state) = self.get("/admin/api/state", &[("Cookie", &cookie)]).await;
    (cookie, state["sites"][0]["api_key"].as_str().unwrap().to_owned())
  }

  async fn tick(&self) {
    while self.app.tick(server::unix_now()).await {}
  }
}

#[tokio::test]
async fn setup_validates_the_code_and_the_wallet() {
  let h = harness().await;
  let response = h.http.get(format!("{}/", h.base)).send().await.unwrap();
  assert_eq!(response.headers()["location"], "/setup");

  let body = |token: &str, view_key: &str, password: &str| {
    json!({ "token": token, "address": h.wallet.address, "view_key": view_key, "password": password })
  };
  let other = TestWallet::random();
  let (status, json, _) = h.post("/setup/api", body("wrong", &h.wallet.view_hex, "correct horse"), &[]).await;
  assert_eq!((status, json["error"].as_str()), (403, Some("bad_token")));
  let (status, json, _) = h.post("/setup/api", body("letmein", &other.view_hex, "correct horse"), &[]).await;
  assert_eq!((status, json["error"].as_str()), (400, Some("view_key_mismatch")));
  let (status, json, _) = h.post("/setup/api", body("letmein", &h.wallet.view_hex, "short"), &[]).await;
  assert_eq!((status, json["error"].as_str()), (400, Some("weak_password")));

  let (_, check, _) = h
    .post("/setup/api/check", json!({ "address": h.wallet.address, "view_key": h.wallet.view_hex }), &[])
    .await;
  assert_eq!(check, json!({ "ok": true, "network": "stagenet" }));

  assert!(!h.app.is_configured());
  h.set_up().await;
  assert!(h.app.is_configured());

  // Setup cannot be run twice.
  let (status, json, _) = h.post("/setup/api", body("letmein", &h.wallet.view_hex, "correct horse"), &[]).await;
  assert_eq!((status, json["error"].as_str()), (409, Some("already_configured")));
}

#[tokio::test]
async fn dashboard_requires_a_session_and_login_checks_the_password() {
  let h = harness().await;
  let (cookie, _) = h.set_up().await;

  assert_eq!(h.get("/admin/api/state", &[]).await.0, 401);
  assert_eq!(h.get("/admin/api/state", &[("Cookie", "xmrgate_session=forged")]).await.0, 401);
  let (status, state) = h.get("/admin/api/state", &[("Cookie", &cookie)]).await;
  assert_eq!(status, 200);
  assert_eq!(state["settings"]["shop_name"], "Test Shop");
  assert_eq!(state["primary_address"], h.wallet.address.as_str());
  // The view key is never sent back to the browser.
  assert!(!state.to_string().contains(&h.wallet.view_hex));

  let (status, json, _) = h.post("/login/api", json!({ "password": "nope" }), &[]).await;
  assert_eq!((status, json["error"].as_str()), (401, Some("wrong_password")));
  let (status, _, headers) = h.post("/login/api", json!({ "password": "correct horse" }), &[]).await;
  assert_eq!(status, 200);
  let fresh = headers["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned();
  assert_eq!(h.get("/admin/api/state", &[("Cookie", &fresh)]).await.0, 200);

  h.post("/logout", json!({}), &[("Cookie", &fresh)]).await;
  assert_eq!(h.get("/admin/api/state", &[("Cookie", &fresh)]).await.0, 401);
}

#[tokio::test]
async fn login_locks_after_repeated_failures() {
  let h = harness().await;
  h.set_up().await;
  for _ in 0 .. 5 {
    assert_eq!(h.post("/login/api", json!({ "password": "nope" }), &[]).await.0, 401);
  }
  let (status, json, _) = h.post("/login/api", json!({ "password": "correct horse" }), &[]).await;
  assert_eq!((status, json["error"].as_str()), (429, Some("locked")));
}

#[tokio::test]
async fn shop_creates_an_invoice_customer_pays_and_the_shop_is_notified() {
  let h = harness().await;
  let (_, key) = h.set_up().await;
  let bearer = format!("Bearer {key}");
  h.tick().await;

  // A tiny shop that records the webhooks it receives.
  let received: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
  let sink = received.clone();
  let shop = serve(Router::new().route(
    "/hook",
    post(move |headers: HeaderMap, body: String| {
      let sink = sink.clone();
      async move {
        let signature = headers["x-xmrgate-signature"].to_str().unwrap().to_owned();
        sink.lock().unwrap().push((signature, body));
      }
    }),
  ))
  .await;

  let order = json!({
    "amount": "19.99", "currency": "EUR", "order_ref": "1042",
    "callback_url": format!("{shop}/hook"), "redirect_url": "https://shop.example/thanks",
  });
  assert_eq!(h.post("/api/v1/invoices", order.clone(), &[]).await.0, 401);
  assert_eq!(h.post("/api/v1/invoices", order.clone(), &[("Authorization", "Bearer wrong")]).await.0, 401);

  let (status, invoice, _) = h.post("/api/v1/invoices", order, &[("Authorization", &bearer)]).await;
  assert_eq!(status, 201);
  // 19.99 EUR at 500 EUR per XMR.
  assert_eq!(invoice["amount_xmr"], "0.03998");
  assert_eq!(invoice["status"], "pending");
  let id = invoice["id"].as_str().unwrap();
  let address = invoice["address"].as_str().unwrap();
  assert_eq!(invoice["checkout_url"], format!("{}/pay/{id}", h.base));
  assert_eq!(invoice["payment_uri"], format!("monero:{address}?tx_amount=0.03998"));

  // The checkout page and its QR code are public, keyed by the unguessable invoice id.
  let page = h.http.get(format!("{}/pay/{id}", h.base)).send().await.unwrap();
  assert_eq!(page.status(), 200);
  assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("script-src 'self'"));
  let qr = h.http.get(format!("{}/pay/{id}/qr.svg", h.base)).send().await.unwrap();
  assert_eq!(qr.headers()["content-type"], "image/svg+xml");
  assert!(qr.text().await.unwrap().starts_with("<?xml"));
  assert_eq!(h.http.get(format!("{}/pay/nope", h.base)).send().await.unwrap().status(), 404);

  // The customer pays; the transaction is first seen in the mempool, then mined.
  let tx = pay_address(address, 39_980_000_000);
  let txid = h.chain.broadcast(tx.clone());
  h.tick().await;
  let (_, status) = h.get(&format!("/pay/{id}/status"), &[]).await;
  assert_eq!((status["status"].as_str(), status["shop_name"].as_str()), (Some("seen"), Some("Test Shop")));

  h.chain.mine(vec![(txid, tx)]);
  h.tick().await;
  h.tick().await;
  let (_, fetched) = h.get(&format!("/api/v1/invoices/{id}"), &[("Authorization", &bearer)]).await;
  assert_eq!((fetched["status"].as_str(), fetched["confirmations"].as_u64()), (Some("confirmed"), Some(1)));

  // The shop received both updates, each signed with the API key.
  let hooks = received.lock().unwrap().clone();
  let statuses: Vec<String> = hooks
    .iter()
    .map(|(signature, body)| {
      assert_eq!(*signature, sign(&key, body.as_bytes()));
      let json: Value = serde_json::from_str(body).unwrap();
      assert_eq!(json["invoice"]["order_ref"], "1042");
      json["invoice"]["status"].as_str().unwrap().to_owned()
    })
    .collect();
  assert_eq!(statuses, ["seen", "confirmed"]);
}

#[tokio::test]
async fn dashboard_serves_the_woocommerce_plugin() {
  let h = harness().await;
  let response = h.http.get(format!("{}/downloads/xmrgate-woocommerce.zip", h.base)).send().await.unwrap();
  assert_eq!(response.headers()["content-type"], "application/zip");
  let bytes = response.bytes().await.unwrap();
  assert_eq!(&bytes[.. 2], b"PK");
  let listing = String::from_utf8_lossy(&bytes);
  for file in ["xmrgate/xmrgate.php", "xmrgate/includes/class-wc-gateway-xmrgate.php", "xmrgate/assets/blocks.js"] {
    assert!(listing.contains(file), "{file} is missing from the plugin zip");
  }
}

#[tokio::test]
async fn invoices_reject_bad_input() {
  let h = harness().await;
  let (cookie, key) = h.set_up().await;
  let bearer = format!("Bearer {key}");
  let auth = [("Authorization", bearer.as_str())];
  let create = |body: Value| h.post("/api/v1/invoices", body, &auth);

  assert_eq!(create(json!({ "amount": "abc", "currency": "EUR" })).await.1["error"], "invalid_amount");
  assert_eq!(create(json!({ "amount": "0", "currency": "XMR" })).await.1["error"], "invalid_invoice");
  assert_eq!(create(json!({ "amount": "-5", "currency": "XMR" })).await.1["error"], "invalid_amount");
  assert_eq!(
    create(json!({ "amount": "1", "currency": "XMR", "callback_url": "javascript:alert(1)" })).await.1["error"],
    "invalid_url"
  );
  // Amounts given as JSON numbers are accepted too, and XMR needs no exchange rate.
  let (status, invoice, _) = create(json!({ "amount": 0.5, "currency": "xmr" })).await;
  assert_eq!((status, invoice["amount_xmr"].as_str()), (201, Some("0.5")));

  // The dashboard can create payment links with its session instead of the API key.
  let (status, link, _) = h
    .post("/admin/api/invoices", json!({ "amount": "10", "currency": "USD" }), &[("Cookie", &cookie)])
    .await;
  assert_eq!((status, link["amount_xmr"].as_str()), (201, Some("0.02")));
  assert_eq!(h.post("/admin/api/invoices", json!({ "amount": "10" }), &[]).await.0, 401);
}

#[tokio::test]
async fn settings_are_validated_and_applied() {
  let h = harness().await;
  let (cookie, key) = h.set_up().await;
  let auth = [("Cookie", cookie.as_str())];

  assert_eq!(h.post("/admin/api/settings", json!({ "confirmations": 3 }), &[]).await.0, 401);
  assert_eq!(h.post("/admin/api/settings", json!({ "confirmations": 500 }), &auth).await.0, 400);
  assert_eq!(h.post("/admin/api/settings", json!({ "nodes": ["ftp://nope"] }), &auth).await.0, 400);
  let (status, _, _) = h
    .post(
      "/admin/api/settings",
      json!({ "confirmations": 3, "expiry_minutes": 30, "public_url": "https://pay.example.com/",
              "nodes": ["https://a.example:18089/", "https://a.example:18089", "http://b.example:18081"] }),
      &auth,
    )
    .await;
  assert_eq!(status, 200);

  let (_, state) = h.get("/admin/api/state", &auth).await;
  assert_eq!(state["settings"]["nodes"], json!(["https://a.example:18089", "http://b.example:18081"]));
  let (_, invoice, _) = h
    .post("/api/v1/invoices", json!({ "amount": "1", "currency": "XMR" }), &[("Authorization", &format!("Bearer {key}"))])
    .await;
  assert_eq!(invoice["confirmations_required"], 3);
  assert_eq!(invoice["expires_at"].as_i64().unwrap() - invoice["created_at"].as_i64().unwrap(), 1800);
  assert!(invoice["checkout_url"].as_str().unwrap().starts_with("https://pay.example.com/pay/"));

  // Replacing the key invalidates the old one.
  let site = state["sites"][0]["id"].as_i64().unwrap();
  assert_eq!(h.post(&format!("/admin/api/sites/{site}/rotate-key"), json!({}), &[]).await.0, 401);
  let (_, rotated, _) = h.post(&format!("/admin/api/sites/{site}/rotate-key"), json!({}), &auth).await;
  let fresh = rotated["api_key"].as_str().unwrap();
  assert_ne!(fresh, key);
  for (key, expected) in [(key.as_str(), 401), (fresh, 201)] {
    let bearer = format!("Bearer {key}");
    let answer = h.post("/api/v1/invoices", json!({ "amount": "1" }), &[("Authorization", &bearer)]).await;
    assert_eq!(answer.0, expected);
  }
}

#[tokio::test]
async fn each_site_has_its_own_key_invoices_and_webhook_signature() {
  let h = harness().await;
  let (cookie, first_key) = h.set_up().await;
  let auth = [("Cookie", cookie.as_str())];
  h.tick().await;

  assert_eq!(h.post("/admin/api/sites", json!({ "name": "Second shop" }), &[]).await.0, 401);
  assert_eq!(h.post("/admin/api/sites", json!({ "name": "  " }), &auth).await.1["error"], "invalid_name");
  let (status, second, _) = h.post("/admin/api/sites", json!({ "name": "Second shop" }), &auth).await;
  assert_eq!(status, 201);
  let second_key = second["api_key"].as_str().unwrap().to_owned();
  assert_ne!(second_key, first_key);

  // Both shops share one webhook receiver here, which records what it gets.
  let received: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
  let sink = received.clone();
  let shop = serve(Router::new().route(
    "/hook",
    post(move |headers: HeaderMap, body: String| {
      let sink = sink.clone();
      async move {
        sink.lock().unwrap().push((headers["x-xmrgate-signature"].to_str().unwrap().to_owned(), body));
      }
    }),
  ))
  .await;

  let order = |reference: &str| json!({ "amount": "1", "order_ref": reference, "callback_url": format!("{shop}/hook") });
  let bearer = |key: &str| format!("Bearer {key}");
  let (_, a, _) = h.post("/api/v1/invoices", order("A-1"), &[("Authorization", &bearer(&first_key))]).await;
  let (_, b, _) = h.post("/api/v1/invoices", order("B-1"), &[("Authorization", &bearer(&second_key))]).await;
  let (a_id, b_id) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());

  // A site cannot read another site's invoice, even knowing its id.
  assert_eq!(h.get(&format!("/api/v1/invoices/{a_id}"), &[("Authorization", &bearer(&first_key))]).await.0, 200);
  assert_eq!(h.get(&format!("/api/v1/invoices/{a_id}"), &[("Authorization", &bearer(&second_key))]).await.0, 404);
  assert_eq!(h.get(&format!("/api/v1/invoices/{b_id}"), &[("Authorization", &bearer(&first_key))]).await.0, 404);

  // The dashboard shows where each invoice came from.
  let (_, state) = h.get("/admin/api/state", &auth).await;
  let source = |id: &str| {
    let invoices = state["invoices"].as_array().unwrap();
    invoices.iter().find(|invoice| invoice["id"] == id).unwrap()["source"].clone()
  };
  assert_eq!((source(a_id), source(b_id)), (json!("Test Shop"), json!("Second shop")));

  // Both get paid. Each webhook is signed with the key of the site that created the invoice.
  h.chain.mine(vec![
    (common::random_hash(), pay_address(a["address"].as_str().unwrap(), 1_000_000_000_000)),
    (common::random_hash(), pay_address(b["address"].as_str().unwrap(), 1_000_000_000_000)),
  ]);
  h.tick().await;
  h.tick().await;
  let hooks = received.lock().unwrap().clone();
  assert_eq!(hooks.len(), 2);
  for (signature, body) in &hooks {
    let json: Value = serde_json::from_str(body).unwrap();
    let key = if json["invoice"]["order_ref"] == "A-1" { &first_key } else { &second_key };
    assert_eq!(*signature, sign(key, body.as_bytes()));
    assert_eq!(json["invoice"]["status"], "confirmed");
  }

  // Removing a site revokes its key and leaves the other untouched.
  let id = second["id"].as_i64().unwrap();
  assert_eq!(h.post(&format!("/admin/api/sites/{id}/delete"), json!({}), &auth).await.0, 200);
  assert_eq!(h.post("/api/v1/invoices", order("B-2"), &[("Authorization", &bearer(&second_key))]).await.0, 401);
  assert_eq!(h.post("/api/v1/invoices", order("A-2"), &[("Authorization", &bearer(&first_key))]).await.0, 201);
  assert_eq!(h.post(&format!("/admin/api/sites/{id}/delete"), json!({}), &auth).await.0, 404);
}

#[tokio::test]
async fn payment_button_creates_an_invoice_only_when_pressed() {
  let h = harness().await;
  let (cookie, _) = h.set_up().await;
  let auth = [("Cookie", cookie.as_str())];
  h.tick().await;

  let button = |body: Value| h.post("/admin/api/buttons", body, &auth);
  assert_eq!(h.post("/admin/api/buttons", json!({ "name": "Tea", "amount": "5" }), &[]).await.0, 401);
  assert_eq!(button(json!({ "name": "", "amount": "5", "currency": "EUR" })).await.1["error"], "invalid_name");
  assert_eq!(button(json!({ "name": "Tea", "amount": "0", "currency": "EUR" })).await.1["error"], "invalid_amount");
  assert_eq!(button(json!({ "name": "Tea", "amount": "x", "currency": "XMR" })).await.1["error"], "invalid_amount");
  assert_eq!(
    button(json!({ "name": "Tea", "amount": "5", "currency": "EUR", "redirect_url": "javascript:1" })).await.1["error"],
    "invalid_url"
  );
  let (status, created, _) = button(json!({
    "name": "Herbal tea", "amount": "25", "currency": "EUR", "redirect_url": "https://shop.example/thanks"
  }))
  .await;
  assert_eq!(status, 201);
  let id = created["id"].as_str().unwrap();
  assert_eq!(created["url"], format!("{}/b/{id}", h.base));

  // Opening the link (as a crawler or a link preview would) shows the page and creates nothing.
  let page = h.http.get(format!("{}/b/{id}", h.base)).send().await.unwrap();
  assert_eq!(page.status(), 200);
  let (_, info) = h.get(&format!("/b/{id}/info"), &[]).await;
  assert_eq!(info, json!({ "name": "Herbal tea", "amount": "25", "currency": "EUR", "shop_name": "Test Shop" }));
  let (_, state) = h.get("/admin/api/state", &auth).await;
  assert_eq!(state["invoices"].as_array().unwrap().len(), 0);
  assert_eq!(state["buttons"][0]["name"], "Herbal tea");

  // Pressing the button creates an invoice and sends the customer to pay it.
  let pressed = h.http.post(format!("{}/b/{id}", h.base)).send().await.unwrap();
  assert_eq!(pressed.status(), 303);
  let location = pressed.headers()["location"].to_str().unwrap().to_owned();
  let invoice_id = location.strip_prefix("/pay/").unwrap();
  let (_, invoice) = h.get(&format!("/pay/{invoice_id}/status"), &[]).await;
  // 25 EUR at 500 EUR per XMR.
  assert_eq!(invoice["amount_xmr"], "0.05");
  assert_eq!(invoice["description"], "Herbal tea");
  assert_eq!(invoice["redirect_url"], "https://shop.example/thanks");

  h.chain.mine(vec![(common::random_hash(), pay_address(invoice["address"].as_str().unwrap(), 50_000_000_000))]);
  h.tick().await;
  let (_, state) = h.get("/admin/api/state", &auth).await;
  assert_eq!(state["invoices"][0]["status"], "confirmed");
  assert_eq!(state["invoices"][0]["source"], "Herbal tea");

  // A public link cannot be used to flood the wallet with unpaid requests.
  let mut refused = 0;
  for _ in 0 .. 40 {
    if h.http.post(format!("{}/b/{id}", h.base)).send().await.unwrap().status() == 429 {
      refused += 1;
    }
  }
  assert_eq!(refused, 10, "30 unpaid requests are allowed at once, the rest are refused");

  // A removed button stops working.
  assert_eq!(h.post(&format!("/admin/api/buttons/{id}/delete"), json!({}), &auth).await.0, 200);
  assert_eq!(h.http.get(format!("{}/b/{id}", h.base)).send().await.unwrap().status(), 404);
  assert_eq!(h.http.post(format!("{}/b/{id}", h.base)).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn dashboard_invoices_cannot_request_webhooks() {
  let h = harness().await;
  let (cookie, _) = h.set_up().await;
  h.tick().await;
  // Webhooks are signed with a site's key; a dashboard invoice has no site.
  let (_, invoice, _) = h
    .post(
      "/admin/api/invoices",
      json!({ "amount": "1", "currency": "XMR", "callback_url": "http://127.0.0.1:1/hook" }),
      &[("Cookie", &cookie)],
    )
    .await;
  h.chain.mine(vec![(common::random_hash(), pay_address(invoice["address"].as_str().unwrap(), 1_000_000_000_000))]);
  h.tick().await;
  let (_, status) = h.get(&format!("/pay/{}/status", invoice["id"].as_str().unwrap()), &[]).await;
  assert_eq!(status["status"], "confirmed");
  assert!(h.app.store_for_tests().due_webhooks(i64::MAX - 1).unwrap().is_empty());
}

#[tokio::test]
async fn developer_guide_shows_the_tested_examples() {
  let h = harness().await;
  let guide = h.http.get(format!("{}/docs", h.base)).send().await.unwrap().text().await.unwrap();
  let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;");
  for file in ["shop.php", "shop.mjs", "shop.py"] {
    let source = std::fs::read_to_string(format!("examples/integrations/{file}")).unwrap();
    assert!(guide.contains(escape(source.trim_end()).as_str()), "{file} in the guide is out of date");
  }
}

#[tokio::test]
async fn database_from_the_first_release_is_upgraded_in_place() {
  // The first release had one key in the settings and no site or button columns.
  let dir = std::env::temp_dir().join(format!("xmrgate-upgrade-{}", hex::encode(common::random_hash())));
  std::fs::create_dir_all(&dir).unwrap();
  let path = dir.join("xmrgate.sqlite");
  let wallet = TestWallet::random();
  {
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
      "CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
       CREATE TABLE invoices (
         id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
         watch_until INTEGER NOT NULL, sub_index INTEGER NOT NULL, address TEXT NOT NULL,
         amount INTEGER NOT NULL, fiat_amount TEXT, fiat_currency TEXT, order_ref TEXT,
         description TEXT, callback_url TEXT, redirect_url TEXT,
         confirmations_required INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
         received INTEGER NOT NULL DEFAULT 0, confirmations INTEGER NOT NULL DEFAULT 0,
         released INTEGER NOT NULL DEFAULT 0);
       CREATE TABLE webhooks (
         id INTEGER PRIMARY KEY AUTOINCREMENT, invoice_id TEXT NOT NULL, url TEXT NOT NULL,
         payload TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
         next_attempt_at INTEGER NOT NULL, delivered_at INTEGER, last_error TEXT);
       INSERT INTO invoices (id, created_at, expires_at, watch_until, sub_index, address, amount,
                             confirmations_required, status, received, confirmations)
       VALUES ('old-invoice', 1, 2, 3, 1, 'addr', 1000, 1, 'confirmed', 1000, 5);",
    )
    .unwrap();
    for (key, value) in [
      ("address", wallet.address.as_str()),
      ("view_key", wallet.view_hex.as_str()),
      ("api_key", "legacy-key"),
      ("shop_name", "Old Shop"),
    ] {
      db.execute("INSERT INTO config (key, value) VALUES (?1, ?2)", [key, value]).unwrap();
    }
  }

  let store = Arc::new(Store::open(&path).unwrap());
  let chain = Arc::new(MockChain::new(10));
  let app = server::App::new(store.clone(), Rates::fixed("500"), None, Some(chain)).unwrap();
  assert!(app.is_configured());

  // The old key became the first site, the old invoice is still there, and new ones work.
  let sites = store.sites().unwrap();
  assert_eq!(sites.len(), 1);
  assert_eq!((sites[0].name.as_str(), sites[0].api_key.as_str()), ("Old Shop", "legacy-key"));
  assert_eq!(store.invoice("old-invoice").unwrap().unwrap().site_id, None);
  let base = serve(server::router(app)).await;
  let http = reqwest::Client::builder().no_proxy().build().unwrap();
  let created = http
    .post(format!("{base}/api/v1/invoices"))
    .header("Authorization", "Bearer legacy-key")
    .json(&json!({ "amount": "1" }))
    .send()
    .await
    .unwrap();
  assert_eq!(created.status(), 201);
  std::fs::remove_dir_all(&dir).unwrap();
}
