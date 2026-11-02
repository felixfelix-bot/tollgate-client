//! The Android Lightning lane's JSON boundary, tested without a JVM.
//!
//! The JNI entry points are thin: they marshal strings and delegate to the
//! `*_json` functions here. Those functions are the contract the Java shell
//! parses, so they are what the tests pin — a JNI symbol that resolves but
//! returns the wrong shape is just as broken as one that does not resolve.

use crate::lightning;

/// JSON for `Core.requestInvoice(gateUrl, amountSat, mintUrl, timeoutMs)`.
///
/// Success mirrors the gate's own fields so the shell can show the price it is
/// about to pay: `{"quote","invoice","amount","mint_url","expiry"}`.
/// Failure is always `{"error":…}` — never a partial object, because a shell
/// that has to guess which half arrived is how a client pays twice.
pub fn request_invoice_json(result: Result<lightning::Invoice, lightning::LightningError>) -> String {
    match result {
        Ok(inv) => format!(
            r#"{{"quote":"{}","invoice":"{}","amount":{},"mint_url":"{}","expiry":{}}}"#,
            escape(&inv.quote),
            escape(&inv.bolt11),
            inv.amount,
            escape(&inv.mint_url),
            inv.expiry
        ),
        Err(e) => format!(r#"{{"error":"{}"}}"#, escape(&e.to_string())),
    }
}

/// JSON for `Core.pollInvoice(gateUrl, quote, timeoutMs)`.
///
/// `{"state","access_granted","allotment","metric"}`. `access_granted` is the
/// only field a shell may treat as permission to say "you are online": a grant
/// notice alone is not access, and the module's own `/session-state` is the
/// authority this field mirrors.
pub fn poll_invoice_json(result: Result<lightning::InvoiceStatus, lightning::LightningError>) -> String {
    match result {
        Ok(st) => {
            let allotment = st
                .allotment
                .map(|a| a.to_string())
                .unwrap_or_else(|| "null".to_string());
            let metric = st
                .metric
                .as_deref()
                .map(|m| format!(r#""{}""#, escape(m)))
                .unwrap_or_else(|| "null".to_string());
            format!(
                r#"{{"state":"{}","access_granted":{},"allotment":{},"metric":{}}}"#,
                escape(&st.state),
                st.access_granted,
                allotment,
                metric
            )
        }
        Err(lightning::LightningError::PaidButNotGranted { message }) => format!(
            r#"{{"state":"paid-but-not-granted","access_granted":false,"retry":true,"error":"{}"}}"#,
            escape(&message)
        ),
        Err(e) => format!(r#"{{"error":"{}"}}"#, escape(&e.to_string())),
    }
}

/// A JSON string body must not be able to break the object it is embedded in.
/// The gate's `error` text and a mint's quote id are both remote input.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoice() -> lightning::Invoice {
        lightning::Invoice {
            quote: "q-1".into(),
            bolt11: "lnbc1xyz".into(),
            amount: 2,
            mint_url: "http://127.0.0.1:3338/".into(),
            expiry: 1234,
        }
    }

    #[test]
    fn an_invoice_carries_every_field_the_shell_shows() {
        let json = request_invoice_json(Ok(invoice()));
        assert!(json.contains(r#""quote":"q-1""#), "{json}");
        assert!(json.contains(r#""invoice":"lnbc1xyz""#), "{json}");
        assert!(json.contains(r#""amount":2"#), "{json}");
        assert!(json.contains(r#""mint_url":"http://127.0.0.1:3338/""#), "{json}");
        assert!(json.contains(r#""expiry":1234"#), "{json}");
        assert!(!json.contains("error"), "{json}");
    }

    #[test]
    fn a_refusal_is_an_error_object_and_nothing_else() {
        let json = request_invoice_json(Err(lightning::LightningError::Refused {
            code: "quotes-full".into(),
            message: "too many".into(),
            retry_after: Some(30),
        }));
        assert!(json.starts_with(r#"{"error":"#), "{json}");
        assert!(json.contains("quotes-full"), "{json}");
        assert!(!json.contains("invoice"), "{json}");
    }

    #[test]
    fn a_paid_but_ungranted_poll_is_retry_not_failure_and_not_access() {
        let json = poll_invoice_json(Err(lightning::LightningError::PaidButNotGranted {
            message: "nds down".into(),
        }));
        assert!(json.contains(r#""access_granted":false"#), "{json}");
        assert!(json.contains(r#""retry":true"#), "{json}");
        assert!(json.contains("paid-but-not-granted"), "{json}");
    }

    #[test]
    fn a_granted_poll_reports_the_allotment_the_gate_gave() {
        let json = poll_invoice_json(Ok(lightning::InvoiceStatus {
            state: "ISSUED".into(),
            access_granted: true,
            allotment: Some(22020096),
            metric: Some("bytes".into()),
        }));
        assert!(json.contains(r#""access_granted":true"#), "{json}");
        assert!(json.contains(r#""allotment":22020096"#), "{json}");
        assert!(json.contains(r#""metric":"bytes""#), "{json}");
    }

    #[test]
    fn an_ungranted_poll_has_no_allotment_rather_than_a_zero() {
        let json = poll_invoice_json(Ok(lightning::InvoiceStatus {
            state: "UNPAID".into(),
            access_granted: false,
            allotment: None,
            metric: None,
        }));
        assert!(json.contains(r#""allotment":null"#), "{json}");
        assert!(json.contains(r#""metric":null"#), "{json}");
        assert!(json.contains(r#""access_granted":false"#), "{json}");
    }

    #[test]
    fn remote_text_cannot_break_out_of_the_json_string() {
        let json = request_invoice_json(Err(lightning::LightningError::Transport(
            "quote \"x\" \\ evil\n{\"ok\":true}".into(),
        )));
        assert!(!json.contains(r#""quote":""#), "{json}");
        // The object still parses as one object with one key.
        let needle = r#"{"error":"#;
        assert!(json.starts_with(needle), "{json}");
        assert_eq!(json.matches(r#""error""#).count(), 1, "{json}");
        assert!(json.contains(r#"\"x\""#), "{json}");
        assert!(json.contains(r#"\\ evil\n"#), "{json}");
    }
}
