//! Exchange rates, for shops that price in a national currency.

use std::{
  collections::HashMap,
  sync::Mutex,
  time::{Duration, Instant},
};

const MAX_AGE: Duration = Duration::from_secs(60);

/// Fetches and briefly caches the price of one XMR in a given currency.
pub struct Rates {
  client: reqwest::Client,
  cache: Mutex<HashMap<String, (String, Instant)>>,
  fixed: Option<String>,
}

impl Rates {
  pub fn new() -> Self {
    let client = reqwest::Client::builder()
      .timeout(Duration::from_secs(10))
      .user_agent("xmrgate")
      .build()
      .expect("default HTTP client");
    Self { client, cache: Mutex::new(HashMap::new()), fixed: None }
  }

  /// Always answer with the same rate. For tests.
  pub fn fixed(rate: &str) -> Self {
    Self { fixed: Some(rate.to_owned()), ..Self::new() }
  }

  /// The price of one XMR in `currency` (an ISO 4217 code), as a decimal string.
  pub async fn rate(&self, currency: &str) -> Result<String, String> {
    let currency = currency.trim().to_ascii_uppercase();
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
      return Err(format!("unknown currency {currency}"));
    }
    if let Some(rate) = &self.fixed {
      return Ok(rate.clone());
    }
    if let Some((rate, at)) = self.cache.lock().unwrap().get(&currency) {
      if at.elapsed() < MAX_AGE {
        return Ok(rate.clone());
      }
    }

    let rate = match self.kraken(&currency).await {
      Ok(rate) => rate,
      Err(_) => self
        .coingecko(&currency)
        .await
        .map_err(|e| format!("no exchange rate available for {currency}: {e}"))?,
    };
    self.cache.lock().unwrap().insert(currency, (rate.clone(), Instant::now()));
    Ok(rate)
  }

  async fn kraken(&self, currency: &str) -> Result<String, String> {
    let url = format!("https://api.kraken.com/0/public/Ticker?pair=XMR{currency}");
    let json: serde_json::Value = self.get(&url).await?;
    let result = json["result"].as_object().ok_or("pair not listed")?;
    let ticker = result.values().next().ok_or("pair not listed")?;
    // "c" is the last trade as [price, volume].
    let price = ticker["c"][0].as_str().ok_or("unexpected answer")?;
    valid_rate(price)
  }

  async fn coingecko(&self, currency: &str) -> Result<String, String> {
    let lower = currency.to_ascii_lowercase();
    let url =
      format!("https://api.coingecko.com/api/v3/simple/price?ids=monero&vs_currencies={lower}");
    let json: serde_json::Value = self.get(&url).await?;
    let price = json["monero"][&lower].as_f64().ok_or("currency not listed")?;
    valid_rate(&format!("{price:.8}"))
  }

  async fn get(&self, url: &str) -> Result<serde_json::Value, String> {
    let response = self.client.get(url).send().await.map_err(|e| e.without_url().to_string())?;
    let response = response.error_for_status().map_err(|e| e.without_url().to_string())?;
    response.json().await.map_err(|e| e.without_url().to_string())
  }
}

impl Default for Rates {
  fn default() -> Self {
    Self::new()
  }
}

fn valid_rate(price: &str) -> Result<String, String> {
  match crate::amount::parse_decimal(price, 8) {
    Some(value) if value > 0 => Ok(price.to_owned()),
    _ => Err("unusable price".to_owned()),
  }
}
