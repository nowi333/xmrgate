//! The HTTP server: shop API, checkout page, setup wizard and dashboard.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    amount::{fiat_to_piconero, format_xmr, parse_xmr},
    chain::{Chain, Node},
    gateway::{invoice_json, Gateway, NewInvoice},
    pool::NodePool,
    rates::Rates,
    store::{Button, Site, Store},
    wallet::{ViewWallet, WalletError},
    Network,
};

const SESSION_COOKIE: &str = "xmrgate_session";
const SESSION_SECS: i64 = 7 * 24 * 3600;
const MAX_LOGIN_FAILURES: u32 = 5;
const LOGIN_LOCK_SECS: i64 = 60;
/// A payment button stops creating invoices while this many of its own are still unpaid, so a
/// public link cannot be used to flood the wallet with subaddresses.
const BUTTON_MAX_OPEN_INVOICES: u32 = 30;
const MAX_SITES: usize = 50;
const MAX_BUTTONS: usize = 200;

fn default_nodes(network: Network) -> &'static [&'static str] {
    match network {
        Network::Mainnet => &[
            "https://node.sethforprivacy.com",
            "http://node.monerodevs.org:18089",
            "http://xmr-node.cakewallet.com:18081",
            "http://nodes.hashvault.pro:18081",
            "http://node.community.rino.io:18081",
        ],
        Network::Stagenet => &[
            "http://node.monerodevs.org:38089",
            "http://stagenet.xmr-tw.org:38081",
            "http://stagenet.community.rino.io:38081",
        ],
        Network::Testnet => &[],
    }
}

/// Replace the dashboard password. For the `--reset-password` command.
pub fn set_password(store: &Store, password: &str) -> Result<(), String> {
    if password.chars().count() < 8 {
        return Err("the password needs at least 8 characters".into());
    }
    let salt = random_token(16);
    store
        .set(
            "password",
            &format!("{salt}:{}", hash_password(password, salt.as_bytes())),
        )
        .map_err(|e| e.to_string())
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_token(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    hex::encode(raw)
}

/// Compare two secrets without leaking where they differ.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn hash_password(password: &str, salt: &[u8]) -> String {
    let mut out = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .expect("fixed-size salt and output are valid");
    hex::encode(out)
}

/// A JSON error with a stable machine-readable code and an English message.
struct ApiError(StatusCode, &'static str, String);

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self(status, code, message.into())
    }
    fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            error.to_string(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1, "message": self.2 }))).into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

pub struct App {
    store: Arc<Store>,
    gateway: RwLock<Option<Arc<Gateway>>>,
    pool: RwLock<Option<Arc<NodePool>>>,
    sessions: Mutex<HashMap<String, i64>>,
    login_failures: Mutex<(u32, i64)>,
    rates: Rates,
    setup_token: String,
    wake: tokio::sync::Notify,
    chain_override: Option<Arc<dyn Chain>>,
    http: reqwest::Client,
}

impl App {
    /// `chain_override` replaces the configured nodes with a fixed chain source. For tests.
    pub fn new(
        store: Arc<Store>,
        rates: Rates,
        setup_token: Option<String>,
        chain_override: Option<Arc<dyn Chain>>,
    ) -> Result<Arc<Self>, String> {
        let app = Arc::new(Self {
            store,
            gateway: RwLock::new(None),
            pool: RwLock::new(None),
            sessions: Mutex::new(HashMap::new()),
            login_failures: Mutex::new((0, 0)),
            rates,
            setup_token: setup_token.unwrap_or_else(|| random_token(8)),
            wake: tokio::sync::Notify::new(),
            chain_override,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("xmrgate")
                .build()
                .map_err(|e| e.to_string())?,
        });
        // Databases from before several sites were supported hold a single key.
        if let Some(key) = app.config("api_key") {
            if app.store.sites().map_err(|e| e.to_string())?.is_empty() {
                let name = app
                    .config("shop_name")
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "My site".into());
                app.store
                    .insert_site(&name, &key, unix_now())
                    .map_err(|e| e.to_string())?;
            }
        }
        app.start_gateway()?;
        Ok(app)
    }

    #[doc(hidden)]
    pub fn store_for_tests(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn is_configured(&self) -> bool {
        self.gateway.read().unwrap().is_some()
    }

    /// The one-time code that authorises the first-run setup.
    pub fn setup_token(&self) -> &str {
        &self.setup_token
    }

    fn config(&self, key: &str) -> Option<String> {
        self.store.get(key).ok().flatten()
    }

    fn config_u32(&self, key: &str, default: u32) -> u32 {
        self.config(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    fn node_urls(&self) -> Vec<String> {
        self.config("nodes")
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn gateway(&self) -> Result<Arc<Gateway>, ApiError> {
        self.gateway.read().unwrap().clone().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "not_configured",
                "setup is not finished",
            )
        })
    }

    fn build_chain(&self) -> Arc<dyn Chain> {
        if let Some(chain) = &self.chain_override {
            return chain.clone();
        }
        let proxy = self.config("tor_proxy").filter(|p| !p.trim().is_empty());
        let nodes = self
            .node_urls()
            .iter()
            .filter_map(|url| {
                let node = Node::new(url, proxy.as_deref()).ok()?;
                Some((node.url().to_owned(), Arc::new(node) as Arc<dyn Chain>))
            })
            .collect();
        let pool = Arc::new(NodePool::new(nodes));
        *self.pool.write().unwrap() = Some(pool.clone());
        pool
    }

    /// Start the engine if a wallet is configured.
    fn start_gateway(&self) -> Result<(), String> {
        let (Some(address), Some(view_key)) = (self.config("address"), self.config("view_key"))
        else {
            return Ok(());
        };
        let wallet = ViewWallet::new(&address, &view_key).map_err(|e| e.to_string())?;
        let gateway = Gateway::new(self.store.clone(), wallet, self.build_chain())
            .map_err(|e| e.to_string())?;
        *self.gateway.write().unwrap() = Some(Arc::new(gateway));
        Ok(())
    }

    /// Run one scan iteration and deliver webhooks. Returns true if more blocks are waiting.
    pub async fn tick(&self, now: i64) -> bool {
        let Some(gateway) = self.gateway.read().unwrap().clone() else {
            return false;
        };
        let more = gateway.tick(now).await.unwrap_or(false);
        let pool = self.pool.read().unwrap().clone();
        if let Some(pool) = pool {
            for notice in pool.take_notices() {
                let _ = self.store.log("nodes", &notice, now);
            }
        }
        let secret_for = |site: Option<i64>| {
            site.and_then(|id| self.store.site(id).ok().flatten())
                .map(|site| site.api_key)
        };
        let _ = gateway.deliver_webhooks(&self.http, secret_for, now).await;
        more
    }

    /// Scan forever. Polls every few seconds while invoices are open, and rarely otherwise.
    pub async fn run(self: Arc<Self>) {
        loop {
            let more = self.tick(unix_now()).await;
            if more {
                continue;
            }
            let idle = self
                .gateway
                .read()
                .unwrap()
                .as_ref()
                .is_none_or(|g| g.health().idle);
            let pause = Duration::from_secs(if idle { 20 } else { 5 });
            let _ = tokio::time::timeout(pause, self.wake.notified()).await;
        }
    }

    fn session_valid(&self, headers: &HeaderMap) -> bool {
        let Some(cookies) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let now = unix_now();
        let sessions = self.sessions.lock().unwrap();
        cookies
            .split(';')
            .filter_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
            .any(|token| sessions.get(token).is_some_and(|expiry| *expiry > now))
    }

    fn open_session(&self, headers: &HeaderMap) -> HeaderValue {
        let token = random_token(32);
        let now = unix_now();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, expiry| *expiry > now);
        sessions.insert(token.clone(), now + SESSION_SECS);
        let secure = if forwarded_https(headers) {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
      "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_SECS}{secure}"
    ))
        .expect("cookie is ASCII")
    }

    fn require_admin(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        if self.session_valid(headers) {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "please sign in",
            ))
        }
    }

    /// The site whose secret key was presented.
    fn require_site(&self, headers: &HeaderMap) -> Result<Site, ApiError> {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .trim();
        let sites = self.store.sites().map_err(ApiError::internal)?;
        // Every key is compared, so timing does not reveal which site, if any, came close.
        let mut found = None;
        for site in sites {
            if !given.is_empty() && same_secret(given, &site.api_key) {
                found = Some(site);
            }
        }
        found.ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or wrong secret key",
            )
        })
    }

    fn base_url(&self, headers: &HeaderMap) -> String {
        if let Some(url) = self.config("public_url").filter(|u| !u.trim().is_empty()) {
            return url.trim().trim_end_matches('/').to_owned();
        }
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");
        let scheme = if forwarded_https(headers) {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{host}")
    }

    /// Create an invoice. `site` is set for API calls, `button` for payment buttons.
    async fn create_invoice(
        &self,
        body: InvoiceRequest,
        headers: &HeaderMap,
        site: Option<&Site>,
        button: Option<&Button>,
    ) -> Result<(crate::store::Invoice, Value), ApiError> {
        let gateway = self.gateway()?;
        let currency = body
            .currency
            .as_deref()
            .unwrap_or("XMR")
            .trim()
            .to_ascii_uppercase();
        let amount_text = match &body.amount {
            Value::String(text) => text.trim().to_owned(),
            Value::Number(number) => number.to_string(),
            _ => return Err(ApiError::bad("invalid_amount", "amount is required")),
        };

        let (amount, fiat_amount, fiat_currency) = if currency == "XMR" {
            let amount = parse_xmr(&amount_text).ok_or_else(|| {
                ApiError::bad("invalid_amount", "amount is not a valid XMR amount")
            })?;
            (amount, None, None)
        } else {
            let rate = self.rates.rate(&currency).await.map_err(|e| {
                ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "rate_unavailable", e)
            })?;
            let amount = fiat_to_piconero(&amount_text, &rate)
                .ok_or_else(|| ApiError::bad("invalid_amount", "amount is not a valid number"))?;
            (amount, Some(amount_text), Some(currency))
        };

        for url in [&body.callback_url, &body.redirect_url]
            .into_iter()
            .flatten()
        {
            if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2000 {
                return Err(ApiError::bad(
                    "invalid_url",
                    "URLs must start with http:// or https://",
                ));
            }
        }
        let clip = |text: Option<String>| text.map(|t| t.chars().take(200).collect::<String>());

        let invoice = gateway
            .create_invoice(
                NewInvoice {
                    amount,
                    fiat_amount,
                    fiat_currency,
                    order_ref: clip(body.order_ref),
                    description: clip(body.description),
                    // Webhooks are signed with a site's key, so only a site can ask for them.
                    callback_url: body.callback_url.filter(|_| site.is_some()),
                    redirect_url: body.redirect_url,
                    confirmations: body
                        .confirmations
                        .unwrap_or_else(|| self.config_u32("confirmations", 1)),
                    ttl_secs: 60
                        * i64::from(
                            body.expires_in_minutes
                                .unwrap_or_else(|| self.config_u32("expiry_minutes", 10)),
                        ),
                    site_id: site.map(|site| site.id),
                    button_id: button.map(|button| button.id.clone()),
                },
                unix_now(),
            )
            .map_err(|e| match e {
                crate::GatewayError::InvalidInvoice(why) => ApiError::bad("invalid_invoice", why),
                other => ApiError::internal(other),
            })?;
        self.wake.notify_one();

        let mut json = invoice_json(&invoice);
        json["checkout_url"] = json!(format!("{}/pay/{}", self.base_url(headers), invoice.id));
        Ok((invoice, json))
    }
}

fn forwarded_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        == Some("https")
}

#[derive(Deserialize, Default)]
struct InvoiceRequest {
    amount: Value,
    currency: Option<String>,
    order_ref: Option<String>,
    description: Option<String>,
    callback_url: Option<String>,
    redirect_url: Option<String>,
    expires_in_minutes: Option<u32>,
    confirmations: Option<u32>,
}

// --- static pages ---

fn page(body: &'static str) -> Response {
    Html(body).into_response()
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn index(State(app): State<Arc<App>>) -> Redirect {
    Redirect::to(if app.is_configured() {
        "/admin"
    } else {
        "/setup"
    })
}

async fn setup_page(State(app): State<Arc<App>>) -> Response {
    if app.is_configured() {
        return Redirect::to("/admin").into_response();
    }
    page(include_str!("../web/setup.html"))
}

async fn login_page(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if !app.is_configured() {
        return Redirect::to("/setup").into_response();
    }
    if app.session_valid(&headers) {
        return Redirect::to("/admin").into_response();
    }
    page(include_str!("../web/login.html"))
}

async fn admin_page(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if !app.is_configured() {
        return Redirect::to("/setup").into_response();
    }
    if !app.session_valid(&headers) {
        return Redirect::to("/login").into_response();
    }
    page(include_str!("../web/admin.html"))
}

async fn checkout_page(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    match app.store.invoice(&id) {
        Ok(Some(_)) => page(include_str!("../web/checkout.html")),
        _ => (
            StatusCode::NOT_FOUND,
            page(include_str!("../web/notfound.html")),
        )
            .into_response(),
    }
}

// --- public endpoints ---

async fn checkout_status(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let invoice = app
        .store
        .invoice(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such invoice"))?;
    let mut json = invoice_json(&invoice);
    json["shop_name"] = json!(app.config("shop_name").unwrap_or_default());
    json["now"] = json!(unix_now());
    Ok(Json(json).into_response())
}

async fn checkout_qr(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let invoice = app
        .store
        .invoice(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such invoice"))?;
    let uri = invoice_json(&invoice)["payment_uri"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let svg = qrcode::QrCode::new(uri.as_bytes())
        .map_err(ApiError::internal)?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        svg,
    )
        .into_response())
}

async fn health(State(app): State<Arc<App>>) -> Response {
    let gateway = app.gateway.read().unwrap().clone();
    let Some(gateway) = gateway else {
        return Json(json!({ "status": "setup_required" })).into_response();
    };
    let health = gateway.health();
    let ok =
        health.last_error.is_none() && health.last_ok_at.is_some_and(|at| unix_now() - at < 300);
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({ "status": if ok { "ok" } else { "degraded" }, "scanner": health })),
    )
        .into_response()
}

// --- shop API ---

async fn api_create_invoice(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<InvoiceRequest>,
) -> ApiResult {
    let site = app.require_site(&headers)?;
    let (_, json) = app
        .create_invoice(body, &headers, Some(&site), None)
        .await?;
    Ok((StatusCode::CREATED, Json(json)).into_response())
}

async fn api_get_invoice(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let site = app.require_site(&headers)?;
    // A site only sees its own invoices.
    let invoice = app
        .store
        .invoice(&id)
        .map_err(ApiError::internal)?
        .filter(|invoice| invoice.site_id == Some(site.id))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such invoice"))?;
    let mut json = invoice_json(&invoice);
    json["checkout_url"] = json!(format!("{}/pay/{}", app.base_url(&headers), invoice.id));
    Ok(Json(json).into_response())
}

// --- setup and sign-in ---

#[derive(Deserialize)]
struct SetupRequest {
    token: String,
    address: String,
    view_key: String,
    password: String,
    shop_name: Option<String>,
}

async fn setup_submit(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<SetupRequest>,
) -> ApiResult {
    if app.is_configured() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_configured",
            "setup is already done",
        ));
    }
    if !same_secret(body.token.trim(), &app.setup_token) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_token",
            "wrong setup code",
        ));
    }
    if body.password.chars().count() < 8 {
        return Err(ApiError::bad(
            "weak_password",
            "the password needs at least 8 characters",
        ));
    }
    let wallet = ViewWallet::new(&body.address, &body.view_key).map_err(|e| {
        let code = match e {
            WalletError::InvalidAddress => "invalid_address",
            WalletError::NotPrimaryAddress => "not_primary_address",
            WalletError::InvalidViewKey => "invalid_view_key",
            WalletError::ViewKeyMismatch => "view_key_mismatch",
            WalletError::InvalidSubaddressIndex => "invalid_address",
        };
        ApiError::bad(code, e.to_string())
    })?;

    let set = |key: &str, value: &str| app.store.set(key, value).map_err(ApiError::internal);
    set("address", &wallet.primary_address())?;
    set("view_key", body.view_key.trim())?;
    set_password(&app.store, &body.password).map_err(ApiError::internal)?;
    let shop_name: String = body
        .shop_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect();
    set("shop_name", &shop_name)?;
    if app.store.sites().map_err(ApiError::internal)?.is_empty() {
        let name = if shop_name.is_empty() {
            "My site"
        } else {
            shop_name.as_str()
        };
        app.store
            .insert_site(name, &random_token(24), unix_now())
            .map_err(ApiError::internal)?;
    }
    if app.config("nodes").is_none() {
        let nodes =
            serde_json::to_string(default_nodes(wallet.network())).map_err(ApiError::internal)?;
        set("nodes", &nodes)?;
    }
    app.start_gateway().map_err(ApiError::internal)?;
    app.wake.notify_one();

    let cookie = app.open_session(&headers);
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

/// Check an address and view key without saving anything, for live feedback in the wizard.
async fn setup_check(Json(body): Json<Value>) -> Response {
    let address = body["address"].as_str().unwrap_or("");
    let view_key = body["view_key"].as_str().unwrap_or("");
    let result = match ViewWallet::new(address, view_key) {
        Ok(wallet) => json!({ "ok": true, "network": network_name(wallet.network()) }),
        Err(WalletError::InvalidAddress) => json!({ "ok": false, "error": "invalid_address" }),
        Err(WalletError::NotPrimaryAddress) => {
            json!({ "ok": false, "error": "not_primary_address" })
        }
        Err(WalletError::InvalidViewKey) => json!({ "ok": false, "error": "invalid_view_key" }),
        Err(_) => json!({ "ok": false, "error": "view_key_mismatch" }),
    };
    Json(result).into_response()
}

fn network_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "mainnet",
        Network::Stagenet => "stagenet",
        Network::Testnet => "testnet",
    }
}

async fn login_submit(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let now = unix_now();
    {
        let failures = app.login_failures.lock().unwrap();
        if failures.0 >= MAX_LOGIN_FAILURES && now < failures.1 + LOGIN_LOCK_SECS {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "locked",
                "too many attempts, wait a minute",
            ));
        }
    }
    let password = body["password"].as_str().unwrap_or("").to_owned();
    let stored = app.config("password").unwrap_or_default();
    let ok = tokio::task::spawn_blocking(move || {
        stored.split_once(':').is_some_and(|(salt, hash)| {
            same_secret(&hash_password(&password, salt.as_bytes()), hash)
        })
    })
    .await
    .unwrap_or(false);

    let mut failures = app.login_failures.lock().unwrap();
    if !ok {
        if now >= failures.1 + LOGIN_LOCK_SECS {
            failures.0 = 0;
        }
        *failures = (failures.0 + 1, now);
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "wrong_password",
            "wrong password",
        ));
    }
    *failures = (0, 0);
    drop(failures);
    let cookie = app.open_session(&headers);
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(cookies) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        let mut sessions = app.sessions.lock().unwrap();
        for token in cookies
            .split(';')
            .filter_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
        {
            sessions.remove(token);
        }
    }
    let clear = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    ([(header::SET_COOKIE, clear)], Json(json!({ "ok": true }))).into_response()
}

// --- dashboard ---

async fn admin_state(State(app): State<Arc<App>>, headers: HeaderMap) -> ApiResult {
    app.require_admin(&headers)?;
    let gateway = app.gateway()?;
    let health = gateway.health();
    let nodes = app
        .pool
        .read()
        .unwrap()
        .as_ref()
        .map(|pool| pool.status())
        .unwrap_or_default();
    let healthy_nodes = nodes.iter().filter(|node| node.healthy).count();
    let (total, paid, received) = app.store.totals().map_err(ApiError::internal)?;
    let base = app.base_url(&headers);

    let mut alerts = vec![];
    if app.chain_override.is_none() {
        if nodes.is_empty() {
            alerts.push("no_nodes");
        } else if health.tip.is_some() && nodes.len() >= 2 && healthy_nodes < 2 {
            alerts.push("few_nodes");
        } else if nodes.len() == 1 {
            alerts.push("single_node");
        }
    }
    match &health.last_error {
        Some(error) if error.contains("unsupported protocol") => alerts.push("upgrade_required"),
        Some(_) => alerts.push("scan_error"),
        None => {}
    }
    if let (Some(tip), Some(scanned)) = (health.tip, health.scanned) {
        if tip > scanned + 10 {
            alerts.push("behind");
        }
    }

    let all_sites = app.store.sites().map_err(ApiError::internal)?;
    let all_buttons = app.store.buttons().map_err(ApiError::internal)?;
    let invoices: Vec<Value> = app
        .store
        .recent_invoices(50)
        .map_err(ApiError::internal)?
        .iter()
        .map(|invoice| {
            let mut json = invoice_json(invoice);
            json["checkout_url"] = json!(format!("{base}/pay/{}", invoice.id));
            // Where the invoice came from: a site, a payment button, or the dashboard itself.
            json["source"] = match (&invoice.site_id, &invoice.button_id) {
                (Some(id), _) => json!(all_sites
                    .iter()
                    .find(|site| site.id == *id)
                    .map(|site| site.name.clone())),
                (_, Some(id)) => json!(all_buttons
                    .iter()
                    .find(|button| button.id == *id)
                    .map(|button| button.name.clone())),
                _ => Value::Null,
            };
            json
        })
        .collect();
    let sites: Vec<Value> = all_sites
        .iter()
        .map(|site| json!({ "id": site.id, "name": site.name, "api_key": site.api_key }))
        .collect();
    let buttons: Vec<Value> = all_buttons
    .iter()
    .map(|button| {
      json!({
        "id": button.id, "name": button.name, "amount": button.amount, "currency": button.currency,
        "redirect_url": button.redirect_url, "url": format!("{base}/b/{}", button.id),
      })
    })
    .collect();

    Ok(Json(json!({
      "network": network_name(gateway.wallet().network()),
      "primary_address": gateway.wallet().primary_address(),
      "gateway_url": base,
      "sites": sites,
      "buttons": buttons,
      "health": health,
      "nodes": nodes,
      "alerts": alerts,
      "totals": { "invoices": total, "paid": paid, "received_xmr": format_xmr(received) },
      "invoices": invoices,
      "events": app.store.recent_events(30).map_err(ApiError::internal)?,
      "settings": {
        "shop_name": app.config("shop_name").unwrap_or_default(),
        "confirmations": app.config_u32("confirmations", 1),
        "expiry_minutes": app.config_u32("expiry_minutes", 10),
        "tor_proxy": app.config("tor_proxy").unwrap_or_default(),
        "public_url": app.config("public_url").unwrap_or_default(),
        "nodes": app.node_urls(),
      },
      "now": unix_now(),
    }))
    .into_response())
}

#[derive(Deserialize)]
struct SettingsRequest {
    shop_name: Option<String>,
    confirmations: Option<u32>,
    expiry_minutes: Option<u32>,
    tor_proxy: Option<String>,
    public_url: Option<String>,
    nodes: Option<Vec<String>>,
}

async fn admin_settings(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<SettingsRequest>,
) -> ApiResult {
    app.require_admin(&headers)?;
    let gateway = app.gateway()?;
    let set = |key: &str, value: &str| app.store.set(key, value).map_err(ApiError::internal);

    if let Some(confirmations) = body.confirmations {
        if confirmations > 100 {
            return Err(ApiError::bad(
                "invalid_setting",
                "confirmations must be between 0 and 100",
            ));
        }
        set("confirmations", &confirmations.to_string())?;
    }
    if let Some(minutes) = body.expiry_minutes {
        if !(5..=7 * 24 * 60).contains(&minutes) {
            return Err(ApiError::bad(
                "invalid_setting",
                "expiry must be between 5 minutes and 7 days",
            ));
        }
        set("expiry_minutes", &minutes.to_string())?;
    }
    if let Some(name) = &body.shop_name {
        set(
            "shop_name",
            &name.trim().chars().take(80).collect::<String>(),
        )?;
    }
    if let Some(url) = &body.public_url {
        let url = url.trim();
        if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(ApiError::bad(
                "invalid_url",
                "the public address must start with http:// or https://",
            ));
        }
        set("public_url", url)?;
    }

    let mut rebuild = false;
    if let Some(proxy) = &body.tor_proxy {
        let proxy = proxy.trim();
        if !proxy.is_empty() && Node::new("http://localhost", Some(proxy)).is_err() {
            return Err(ApiError::bad(
                "invalid_proxy",
                "the Tor proxy must look like 127.0.0.1:9050",
            ));
        }
        set("tor_proxy", proxy)?;
        rebuild = true;
    }
    if let Some(nodes) = &body.nodes {
        let mut urls: Vec<String> = vec![];
        for url in nodes
            .iter()
            .map(|url| url.trim())
            .filter(|url| !url.is_empty())
        {
            let node = Node::new(url, None).map_err(|_| {
                ApiError::bad("invalid_node", format!("{url} is not a valid node address"))
            })?;
            if !urls.iter().any(|known| known == node.url()) {
                urls.push(node.url().to_owned());
            }
        }
        if urls.len() > 10 {
            return Err(ApiError::bad("invalid_node", "at most 10 nodes"));
        }
        set(
            "nodes",
            &serde_json::to_string(&urls).map_err(ApiError::internal)?,
        )?;
        rebuild = true;
    }
    if rebuild {
        gateway.set_chain(app.build_chain());
        app.wake.notify_one();
    }
    Ok(Json(json!({ "ok": true })).into_response())
}

async fn admin_create_invoice(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<InvoiceRequest>,
) -> ApiResult {
    app.require_admin(&headers)?;
    let (_, json) = app.create_invoice(body, &headers, None, None).await?;
    Ok((StatusCode::CREATED, Json(json)).into_response())
}

async fn admin_add_site(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    app.require_admin(&headers)?;
    let name: String = body["name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect();
    if name.is_empty() {
        return Err(ApiError::bad("invalid_name", "give the site a name"));
    }
    if app.store.sites().map_err(ApiError::internal)?.len() >= MAX_SITES {
        return Err(ApiError::bad("too_many", "too many sites"));
    }
    let site = app
        .store
        .insert_site(&name, &random_token(24), unix_now())
        .map_err(ApiError::internal)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": site.id, "name": site.name, "api_key": site.api_key })),
    )
        .into_response())
}

/// Replace a site's secret key. The old key stops working at once.
async fn admin_rotate_site_key(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult {
    app.require_admin(&headers)?;
    let key = random_token(24);
    if !app
        .store
        .set_site_key(id, &key)
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such site",
        ));
    }
    Ok(Json(json!({ "api_key": key })).into_response())
}

/// Remove a site. Its key stops working; its past invoices are kept.
async fn admin_delete_site(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult {
    app.require_admin(&headers)?;
    if !app.store.delete_site(id).map_err(ApiError::internal)? {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such site",
        ));
    }
    Ok(Json(json!({ "ok": true })).into_response())
}

#[derive(Deserialize)]
struct ButtonRequest {
    name: String,
    amount: Value,
    currency: Option<String>,
    redirect_url: Option<String>,
}

async fn admin_add_button(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<ButtonRequest>,
) -> ApiResult {
    app.require_admin(&headers)?;
    let name: String = body.name.trim().chars().take(80).collect();
    if name.is_empty() {
        return Err(ApiError::bad("invalid_name", "say what the button sells"));
    }
    let amount = match &body.amount {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    };
    let currency = body
        .currency
        .as_deref()
        .unwrap_or("XMR")
        .trim()
        .to_ascii_uppercase();
    let valid = if currency == "XMR" {
        parse_xmr(&amount).is_some_and(|value| value > 0)
    } else {
        currency.len() == 3
            && currency.bytes().all(|b| b.is_ascii_uppercase())
            && crate::amount::parse_decimal(&amount, 8).is_some_and(|value| value > 0)
    };
    if !valid {
        return Err(ApiError::bad(
            "invalid_amount",
            "amount is not a valid price",
        ));
    }
    let redirect_url = body
        .redirect_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty());
    if let Some(url) = &redirect_url {
        if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2000 {
            return Err(ApiError::bad(
                "invalid_url",
                "URLs must start with http:// or https://",
            ));
        }
    }
    if app.store.buttons().map_err(ApiError::internal)?.len() >= MAX_BUTTONS {
        return Err(ApiError::bad("too_many", "too many payment buttons"));
    }
    let button = Button {
        id: random_token(12),
        name,
        amount,
        currency,
        redirect_url,
        created_at: unix_now(),
    };
    app.store
        .insert_button(&button)
        .map_err(ApiError::internal)?;
    let url = format!("{}/b/{}", app.base_url(&headers), button.id);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": button.id, "url": url })),
    )
        .into_response())
}

async fn admin_delete_button(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    app.require_admin(&headers)?;
    if !app.store.delete_button(&id).map_err(ApiError::internal)? {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such button",
        ));
    }
    Ok(Json(json!({ "ok": true })).into_response())
}

// --- payment buttons (public) ---

async fn button_page(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    match app.store.button(&id) {
        Ok(Some(_)) => page(include_str!("../web/button.html")),
        _ => (
            StatusCode::NOT_FOUND,
            page(include_str!("../web/notfound.html")),
        )
            .into_response(),
    }
}

async fn button_info(State(app): State<Arc<App>>, Path(id): Path<String>) -> ApiResult {
    let button = app
        .store
        .button(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such payment button")
        })?;
    Ok(Json(json!({
      "name": button.name, "amount": button.amount, "currency": button.currency,
      "shop_name": app.config("shop_name").unwrap_or_default(),
    }))
    .into_response())
}

/// The customer pressed the button: create an invoice and send them to its payment page.
///
/// This is a POST so that link previews and crawlers, which only GET, never create invoices.
async fn button_pay(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let button = app
        .store
        .button(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such payment button")
        })?;
    let open = app
        .store
        .open_invoices_for_button(&id, unix_now())
        .map_err(ApiError::internal)?;
    if open >= BUTTON_MAX_OPEN_INVOICES {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "too many unpaid payment requests are open for this button, try again in a few minutes",
        ));
    }
    let request = InvoiceRequest {
        amount: json!(button.amount),
        currency: Some(button.currency.clone()),
        description: Some(button.name.clone()),
        redirect_url: button.redirect_url.clone(),
        ..Default::default()
    };
    let (invoice, _) = app
        .create_invoice(request, &headers, None, Some(&button))
        .await?;
    Ok(Redirect::to(&format!("/pay/{}", invoice.id)).into_response())
}

/// Try a node address before adding it.
async fn admin_test_node(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    app.require_admin(&headers)?;
    let url = body["url"].as_str().unwrap_or("");
    let proxy = app.config("tor_proxy").filter(|p| !p.trim().is_empty());
    let node = Node::new(url, proxy.as_deref())
        .map_err(|e| ApiError::bad("invalid_node", e.to_string()))?;
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(20), node.tip()).await;
    Ok(
    Json(match result {
      Ok(Ok(height)) => json!({ "ok": true, "height": height, "latency_ms": start.elapsed().as_millis() as u64 }),
      Ok(Err(e)) => json!({ "ok": false, "message": e.to_string() }),
      Err(_) => json!({ "ok": false, "message": "timed out" }),
    })
    .into_response(),
  )
}

/// The WooCommerce plugin, packed at build time.
async fn plugin_download() -> Response {
    static ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/xmrgate-woocommerce.zip"));
    (
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"xmrgate-woocommerce.zip\"",
            ),
        ],
        ZIP,
    )
        .into_response()
}

/// The PrestaShop module, packed at build time.
async fn prestashop_download() -> Response {
    static ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/xmrgate-prestashop.zip"));
    (
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"xmrgate-prestashop.zip\"",
            ),
        ],
        ZIP,
    )
        .into_response()
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "Content-Security-Policy",
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
       frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    headers.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    response
}

/// Build the HTTP routes.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/setup", get(setup_page))
        .route("/setup/api", post(setup_submit))
        .route("/setup/api/check", post(setup_check))
        .route("/login", get(login_page))
        .route("/login/api", post(login_submit))
        .route("/logout", post(logout))
        .route("/admin", get(admin_page))
        .route("/admin/api/state", get(admin_state))
        .route("/admin/api/settings", post(admin_settings))
        .route("/admin/api/invoices", post(admin_create_invoice))
        .route("/admin/api/sites", post(admin_add_site))
        .route(
            "/admin/api/sites/{id}/rotate-key",
            post(admin_rotate_site_key),
        )
        .route("/admin/api/sites/{id}/delete", post(admin_delete_site))
        .route("/admin/api/buttons", post(admin_add_button))
        .route("/admin/api/buttons/{id}/delete", post(admin_delete_button))
        .route("/b/{id}", get(button_page).post(button_pay))
        .route("/b/{id}/info", get(button_info))
        .route(
            "/docs",
            get(|| async { page(include_str!("../web/docs.html")) }),
        )
        .route("/admin/api/test-node", post(admin_test_node))
        .route("/api/v1/invoices", post(api_create_invoice))
        .route("/api/v1/invoices/{id}", get(api_get_invoice))
        .route("/pay/{id}", get(checkout_page))
        .route("/pay/{id}/status", get(checkout_status))
        .route("/pay/{id}/qr.svg", get(checkout_qr))
        .route("/health", get(health))
        .route("/downloads/xmrgate-woocommerce.zip", get(plugin_download))
        .route(
            "/downloads/xmrgate-prestashop.zip",
            get(prestashop_download),
        )
        .route(
            "/assets/app.css",
            get(|| async { asset("text/css; charset=utf-8", include_str!("../web/app.css")) }),
        )
        .route(
            "/assets/common.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/common.js"),
                )
            }),
        )
        .route(
            "/assets/setup.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/setup.js"),
                )
            }),
        )
        .route(
            "/assets/login.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/login.js"),
                )
            }),
        )
        .route(
            "/assets/admin.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/admin.js"),
                )
            }),
        )
        .route(
            "/assets/button.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/button.js"),
                )
            }),
        )
        .route(
            "/assets/docs.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/docs.js"),
                )
            }),
        )
        .route(
            "/assets/checkout.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/checkout.js"),
                )
            }),
        )
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}
