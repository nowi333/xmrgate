//! A self-contained demo on a simulated chain, to look at every screen without real funds.
//!
//! Run `cargo run --example demo`, then open the printed addresses.

#[path = "../tests/common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{pay_address, MockChain, TestWallet};
use serde_json::{json, Value};
use xmrgate::{rates::Rates, server, Store};

#[tokio::main]
async fn main() {
  let chain = Arc::new(MockChain::new(2_000));
  let store = Arc::new(Store::in_memory().unwrap());
  let app = server::App::new(store, Rates::fixed("500"), Some("demo".into()), Some(chain.clone())).unwrap();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:3999").await.unwrap();
  let base = "http://127.0.0.1:3999";
  let router = server::router(app.clone());
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

  // A control port stands in for the customer's wallet and for miners:
  //   POST /pay  {"address": "...", "piconero": 123}   broadcast a payment
  //   POST /mine                                       mine the mempool into a block
  let (pay_chain, mine_chain) = (chain.clone(), chain.clone());
  let control = axum::Router::new()
    .route(
      "/pay",
      axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
        let chain = pay_chain.clone();
        async move {
          let tx = pay_address(body["address"].as_str().unwrap(), body["piconero"].as_u64().unwrap());
          hex::encode(chain.broadcast(tx))
        }
      }),
    )
    .route(
      "/mine",
      axum::routing::post(move || {
        let chain = mine_chain.clone();
        async move {
          chain.mine_mempool();
          chain.height().to_string()
        }
      }),
    );
  let control_listener = tokio::net::TcpListener::bind("127.0.0.1:3998").await.unwrap();
  tokio::spawn(async move { axum::serve(control_listener, control).await.unwrap() });
  if std::env::args().any(|arg| arg == "--fresh") {
    tokio::spawn(app.clone().run());
    let wallet = TestWallet::random();
    println!("address: {}", wallet.address);
    println!("viewkey: {}", wallet.view_hex);
    println!("setup: {base}/setup?code=demo");
    println!("control: http://127.0.0.1:3998 (POST /pay, POST /mine)");
    std::future::pending::<()>().await;
  }

  let wallet = TestWallet::random();
  let http = reqwest::Client::builder().no_proxy().build().unwrap();
  let setup = http
    .post(format!("{base}/setup/api"))
    .json(&json!({ "token": "demo", "address": wallet.address, "view_key": wallet.view_hex,
                   "password": "demo-password", "shop_name": "Green House" }))
    .send()
    .await
    .unwrap();
  let cookie = setup.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned();
  app.tick(server::unix_now()).await;

  let create = |body: Value| {
    let (http, cookie) = (http.clone(), cookie.clone());
    async move {
      let response =
        http.post(format!("{base}/admin/api/invoices")).header("Cookie", cookie).json(&body).send().await.unwrap();
      response.json::<Value>().await.unwrap()
    }
  };
  let piconero = |invoice: &Value| invoice["amount_piconero"].as_u64().unwrap();
  let address = |invoice: &Value| invoice["address"].as_str().unwrap().to_owned();

  let paid = create(json!({ "amount": "49.90", "currency": "EUR", "order_ref": "1041" })).await;
  let confirming = create(json!({ "amount": "120", "currency": "EUR", "order_ref": "1042", "confirmations": 3 })).await;
  let partial = create(json!({ "amount": "35", "currency": "EUR", "order_ref": "1043" })).await;
  let expired = create(json!({ "amount": "15", "currency": "EUR", "order_ref": "1044", "expires_in_minutes": 1 })).await;
  let waiting = create(json!({ "amount": "19.99", "currency": "EUR", "order_ref": "1045",
                               "redirect_url": "https://example.com/thanks" })).await;

  // A second website and two payment buttons, to show the rest of the dashboard.
  let admin_post = |path: &'static str, body: Value| {
    let (http, cookie) = (http.clone(), cookie.clone());
    async move {
      http.post(format!("{base}{path}")).header("Cookie", cookie).json(&body).send().await.unwrap();
    }
  };
  admin_post("/admin/api/sites", json!({ "name": "Woodshop (PrestaShop)" })).await;
  admin_post("/admin/api/buttons", json!({ "name": "Ebook: Urban Gardening", "amount": "12", "currency": "EUR" })).await;
  admin_post("/admin/api/buttons", json!({ "name": "Donation", "amount": "0.05", "currency": "XMR" })).await;

  chain.mine(vec![
    (common::random_hash(), pay_address(&address(&paid), piconero(&paid))),
    (common::random_hash(), pay_address(&address(&confirming), piconero(&confirming))),
    (common::random_hash(), pay_address(&address(&partial), piconero(&partial) / 2)),
  ]);
  app.tick(server::unix_now()).await;

  println!("cookie: {cookie}");
  for (name, invoice) in
    [("paid", &paid), ("confirming", &confirming), ("partial", &partial), ("expired", &expired), ("waiting", &waiting)]
  {
    println!("{name}: {}", invoice["checkout_url"].as_str().unwrap());
  }
  println!("dashboard: {base}/admin (password: demo-password)");
  tokio::spawn(app.clone().run());
  std::future::pending::<()>().await;
}
