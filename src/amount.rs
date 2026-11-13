//! Conversions between piconero (the atomic unit) and decimal XMR strings.

pub const PICONERO_PER_XMR: u64 = 1_000_000_000_000;

/// Format piconero as a decimal XMR string without trailing zeros, such as `0.25`.
pub fn format_xmr(piconero: u64) -> String {
  let whole = piconero / PICONERO_PER_XMR;
  let frac = piconero % PICONERO_PER_XMR;
  if frac == 0 {
    return whole.to_string();
  }
  let frac = format!("{frac:012}");
  format!("{whole}.{}", frac.trim_end_matches('0'))
}

/// Parse a non-negative decimal string with up to `decimals` fractional digits into an integer
/// count of the smallest unit. Returns `None` on malformed input or overflow.
pub fn parse_decimal(text: &str, decimals: u32) -> Option<u64> {
  let text = text.trim();
  let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
  if whole.is_empty() && frac.is_empty() {
    return None;
  }
  if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  if frac.len() > decimals as usize {
    return None;
  }
  let scale = 10u64.checked_pow(decimals)?;
  let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
  let frac: u64 =
    if frac.is_empty() { 0 } else { format!("{frac:0<width$}", width = decimals as usize).parse().ok()? };
  whole.checked_mul(scale)?.checked_add(frac)
}

/// Parse a decimal XMR string into piconero.
pub fn parse_xmr(text: &str) -> Option<u64> {
  parse_decimal(text, 12)
}

/// Convert a fiat amount to piconero, rounding up so the merchant is never short.
///
/// `fiat` and `rate` (fiat per XMR) are decimal strings. Fiat is read with up to 8 decimals so
/// that zero-decimal and three-decimal currencies both work.
pub fn fiat_to_piconero(fiat: &str, rate: &str) -> Option<u64> {
  let fiat = parse_decimal(fiat, 8)? as u128;
  let rate = parse_decimal(rate, 8)? as u128;
  if rate == 0 {
    return None;
  }
  let piconero = (fiat * PICONERO_PER_XMR as u128).div_ceil(rate);
  u64::try_from(piconero).ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn formats() {
    assert_eq!(format_xmr(0), "0");
    assert_eq!(format_xmr(PICONERO_PER_XMR), "1");
    assert_eq!(format_xmr(250_000_000_000), "0.25");
    assert_eq!(format_xmr(1), "0.000000000001");
    assert_eq!(format_xmr(12_345_678_900_000), "12.3456789");
  }

  #[test]
  fn parses() {
    assert_eq!(parse_xmr("1"), Some(PICONERO_PER_XMR));
    assert_eq!(parse_xmr("0.25"), Some(250_000_000_000));
    assert_eq!(parse_xmr(".5"), Some(500_000_000_000));
    assert_eq!(parse_xmr("0.000000000001"), Some(1));
    assert_eq!(parse_xmr("0.0000000000001"), None);
    assert_eq!(parse_xmr("-1"), None);
    assert_eq!(parse_xmr("1e3"), None);
    assert_eq!(parse_xmr(""), None);
    assert_eq!(parse_xmr("99999999999999999999"), None);
    for value in [0, 1, 999, PICONERO_PER_XMR - 1, 18_000_000 * PICONERO_PER_XMR] {
      assert_eq!(parse_xmr(&format_xmr(value)), Some(value));
    }
  }

  #[test]
  fn converts_fiat_rounding_up() {
    // 50 EUR at 500 EUR/XMR is exactly 0.1 XMR.
    assert_eq!(fiat_to_piconero("50", "500"), Some(100_000_000_000));
    assert_eq!(fiat_to_piconero("19.99", "493.95"), Some(40_469_683_167));
    // 1 unit at 3 per XMR does not divide evenly: round up.
    assert_eq!(fiat_to_piconero("1", "3"), Some(333_333_333_334));
    assert_eq!(fiat_to_piconero("1", "0"), None);
    assert_eq!(fiat_to_piconero("abc", "3"), None);
  }
}
