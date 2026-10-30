//! The second funding lane: the gate creates the invoice, the customer pays it,
//! and the gate grants access — **no Cashu token ever reaches the client**.
//!
//! Wire contract (tollgate-module-basic-go `src/main.go`):
//!   POST /ln-invoice {"amount":N,"mint_url":"…"}
//!        → 200 {status:1, quote, invoice, mint_url, amount, expiry, state}
//!        → 400/429 {status:0, error, code, retry_after}
//!   GET  /ln-invoice?quote=<id>
//!        → 200 {status:1, state, access_granted, allotment, metric}
//!        → 503 code=access-grant-failed  ← PAID but the gate did not open.
//!          That is not a lookup failure and it is emphatically not a reason to
//!          pay again; it is a retry-later, so it gets its own variant here.
//!
//! Identity is the socket, on both calls: a client must reach the gate from the
//! address it wants granted. There is no MAC parameter that is honoured.

use crate::http;
use crate::json::{field_bool, field_str, field_u64};

#[derive(Debug, Clone, PartialEq)]
pub struct Invoice {
    pub quote: String,
    pub bolt11: String,
    pub amount: u64,
    pub mint_url: String,
    pub expiry: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InvoiceStatus {
    pub state: String,
    pub access_granted: bool,
    pub allotment: Option<u64>,
    pub metric: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LightningError {
    /// The gate refused to create the quote. `retry_after` is the gate's own hint.
    Refused {
        code: String,
        message: String,
        retry_after: Option<u64>,
    },
    /// Paid, but the gate could not open yet. Do NOT pay again.
    PaidButNotGranted {
        message: String,
    },
    Transport(String),
    Malformed(String),
}

impl std::fmt::Display for LightningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LightningError::Refused {
                code,
                message,
                retry_after,
            } => {
                write!(f, "gate refused ({code}): {message}")?;
                if let Some(r) = retry_after {
                    write!(f, " [retry after {r}s]")?;
                }
                Ok(())
            }
            LightningError::PaidButNotGranted { message } => {
                write!(f, "paid but not granted: {message}")
            }
            LightningError::Transport(e) => write!(f, "transport: {e}"),
            LightningError::Malformed(e) => write!(f, "malformed answer: {e}"),
        }
    }
}

pub trait LightningPayer {
    /// Pay a BOLT11 invoice and return an opaque proof (preimage, txid, "manual").
    fn pay(&mut self, bolt11: &str, amount_sat: u64) -> Result<String, String>;
}

fn invoice_url(gate_url: &str) -> String {
    format!("{}/ln-invoice", gate_url.trim_end_matches('/'))
}

pub fn request_invoice(
    gate_url: &str,
    amount_sat: u64,
    mint_url: &str,
    timeout_ms: u64,
) -> Result<Invoice, LightningError> {
    let body = format!(r#"{{"amount":{amount_sat},"mint_url":"{mint_url}"}}"#);
    let (status, resp) = http::request_with_headers(
        "POST",
        &invoice_url(gate_url),
        Some(&body),
        timeout_ms,
        &[("Content-Type", "application/json")],
    )
    .map_err(LightningError::Transport)?;

    if status != 200 {
        return Err(LightningError::Refused {
            code: field_str(&resp, "code").unwrap_or_else(|| format!("http-{status}")),
            message: field_str(&resp, "error").unwrap_or_default(),
            retry_after: field_u64(&resp, "retry_after"),
        });
    }
    let quote =
        field_str(&resp, "quote").ok_or_else(|| LightningError::Malformed("no quote".into()))?;
    let bolt11 = field_str(&resp, "invoice")
        .ok_or_else(|| LightningError::Malformed("no invoice".into()))?;
    Ok(Invoice {
        quote,
        bolt11,
        amount: field_u64(&resp, "amount").unwrap_or(amount_sat),
        mint_url: field_str(&resp, "mint_url").unwrap_or_else(|| mint_url.to_string()),
        expiry: field_u64(&resp, "expiry").unwrap_or(0),
    })
}

pub fn poll_status(
    gate_url: &str,
    quote: &str,
    timeout_ms: u64,
) -> Result<InvoiceStatus, LightningError> {
    let url = format!("{}?quote={quote}", invoice_url(gate_url));
    let (status, resp) =
        http::request("GET", &url, None, timeout_ms).map_err(LightningError::Transport)?;

    let code = field_str(&resp, "code").unwrap_or_default();
    if status == 503 && code == "access-grant-failed" {
        return Err(LightningError::PaidButNotGranted {
            message: field_str(&resp, "error").unwrap_or_default(),
        });
    }
    if status != 200 {
        return Err(LightningError::Refused {
            code: if code.is_empty() {
                format!("http-{status}")
            } else {
                code
            },
            message: field_str(&resp, "error").unwrap_or_default(),
            retry_after: field_u64(&resp, "retry_after"),
        });
    }
    Ok(InvoiceStatus {
        state: field_str(&resp, "state").unwrap_or_else(|| "UNKNOWN".into()),
        access_granted: field_bool(&resp, "access_granted").unwrap_or(false),
        allotment: field_u64(&resp, "allotment"),
        metric: field_str(&resp, "metric"),
    })
}

/// Request → pay → poll. `poll_interval_ms` of 0 is allowed (tests); production
/// should use ~1 s. Returns the first status with `access_granted == true`.
pub fn buy(
    gate_url: &str,
    amount_sat: u64,
    mint_url: &str,
    payer: &mut dyn LightningPayer,
    timeout_ms: u64,
    poll_interval_ms: u64,
    max_polls: u32,
) -> Result<InvoiceStatus, LightningError> {
    let invoice = request_invoice(gate_url, amount_sat, mint_url, timeout_ms)?;
    payer
        .pay(&invoice.bolt11, invoice.amount)
        .map_err(LightningError::Transport)?;

    let mut last = InvoiceStatus {
        state: "PENDING".into(),
        access_granted: false,
        allotment: None,
        metric: None,
    };
    for _ in 0..max_polls {
        if poll_interval_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(poll_interval_ms));
        }
        match poll_status(gate_url, &invoice.quote, timeout_ms) {
            Ok(s) => {
                last = s;
                if last.access_granted {
                    return Ok(last);
                }
            }
            Err(LightningError::PaidButNotGranted { .. }) => {
                // Keep polling: the module retries the grant by itself.
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(LightningError::PaidButNotGranted {
        message: format!(
            "no grant after {max_polls} polls (last state {})",
            last.state
        ),
    })
}
