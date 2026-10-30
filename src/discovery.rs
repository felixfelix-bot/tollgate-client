//! Parse a TollGate v1 advertisement (kind 10021).
//!
//! The grammar is the one `tollgate-module-basic-go` emits in
//! `CreateAdvertisement`:
//!
//! ```text
//! ["metric", "milliseconds" | "bytes"]
//! ["step_size", "<uint>"]
//! ["tips", "1", "2"]
//! ["price_per_step", "cashu", "<price>", "<unit>", "<mint_url>", "<min_steps>"]
//! ```
//!
//! Stricter than the module's own validator on purpose, because this side pays:
//! an advert whose price cannot be turned into a *rate* (no metric, no step
//! size) is rejected, and an offer that cannot be paid (wrong asset type, zero
//! price, no unit, no mint) is dropped rather than guessed at.
//!
//! The parser is dependency-free: the shell (Kotlin/Debian) decodes the JSON and
//! hands over `kind` + tags, so the core needs no serde and stays portable.

use crate::scoring::{Metric, Offer};

pub const TOLLGATE_ADVERTISEMENT_KIND: u64 = 10021;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    WrongKind(u64),
    MissingStepSize,
    UnknownMetric(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::WrongKind(k) => write!(f, "event kind {k} is not an advertisement ({TOLLGATE_ADVERTISEMENT_KIND})"),
            ParseError::MissingStepSize => write!(f, "advert has no usable step_size (missing, zero or non-numeric)"),
            ParseError::UnknownMetric(m) => write!(f, "advert metric {m:?} cannot be priced (expected milliseconds or bytes)"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Advertisement {
    pub metric: Metric,
    pub step_size: u64,
    pub offers: Vec<Offer>,
}

impl Advertisement {
    /// The gateway identity comes from the network (BSSID / portal host), never
    /// from the advert, so the caller stamps it after parsing.
    pub fn stamp_gateway(&mut self, gateway_id: &str) {
        for o in &mut self.offers {
            o.gateway_id = gateway_id.to_string();
        }
    }
}

/// `sats` is an accepted alias on the wire; everything else is preserved so an
/// unknown unit is visible rather than silently coerced.
fn normalise_unit(raw: &str) -> String {
    let t = raw.trim();
    match t {
        "sats" => "sat".to_string(),
        other => other.to_string(),
    }
}

struct RawOffer {
    unit: String,
    price_per_step: u64,
    mint_url: String,
    min_steps: u64,
}

pub fn parse_advertisement(kind: u64, tags: &[Vec<String>]) -> Result<Advertisement, ParseError> {
    if kind != TOLLGATE_ADVERTISEMENT_KIND {
        return Err(ParseError::WrongKind(kind));
    }

    let mut metric: Option<Metric> = None;
    let mut step_size: Option<u64> = None;
    let mut raw_offers: Vec<RawOffer> = Vec::new();

    for tag in tags {
        let Some(name) = tag.first().map(String::as_str) else {
            continue;
        };
        match name {
            "metric" => {
                let value = tag.get(1).map(String::as_str).unwrap_or("");
                metric = Some(match value {
                    "milliseconds" => Metric::Milliseconds,
                    "bytes" => Metric::Bytes,
                    other => return Err(ParseError::UnknownMetric(other.to_string())),
                });
            }
            "step_size" => {
                step_size = Some(
                    tag.get(1)
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .unwrap_or(0),
                );
            }
            "price_per_step" => {
                if tag.len() < 6 {
                    continue;
                }
                if tag[1].trim() != "cashu" {
                    continue; // a real offer, just not one this client can pay
                }
                let Ok(price) = tag[2].trim().parse::<u64>() else {
                    continue;
                };
                if price == 0 {
                    continue;
                }
                let unit = normalise_unit(&tag[3]);
                if unit.is_empty() {
                    continue;
                }
                let mint_url = tag[4].trim().to_string();
                if mint_url.is_empty() {
                    continue;
                }
                let min_steps = tag[5]
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(1)
                    .max(1);
                raw_offers.push(RawOffer {
                    unit,
                    price_per_step: price,
                    mint_url,
                    min_steps,
                });
            }
            _ => {}
        }
    }

    let step_size = step_size.filter(|s| *s > 0).ok_or(ParseError::MissingStepSize)?;
    let metric = metric.ok_or_else(|| ParseError::UnknownMetric("<missing>".to_string()))?;

    let mut offers: Vec<Offer> = Vec::with_capacity(raw_offers.len());
    for raw in raw_offers {
        let candidate = Offer {
            gateway_id: String::new(),
            unit: raw.unit,
            metric,
            price_per_step: raw.price_per_step,
            step_size,
            min_steps: raw.min_steps,
        };
        let duplicate = offers.iter().any(|o| {
            o.unit == candidate.unit
                && o.price_per_step == candidate.price_per_step
                && o.step_size == candidate.step_size
                && o.min_steps == candidate.min_steps
                && o.gateway_id == candidate.gateway_id
        });
        // `raw.mint_url` is part of an offer's identity for the wallet, which is
        // why an empty one is dropped above; the field lands on `Offer` with the
        // wallet module rather than being carried unused here.
        let _ = &raw.mint_url;
        if !duplicate {
            offers.push(candidate);
        }
    }
    // The mint URL is the offer's identity for a wallet, but `Offer` carries no
    // mint field yet (the wallet module will add it); keep it in the error-free
    // path only.

    Ok(Advertisement {
        metric,
        step_size,
        offers,
    })
}
