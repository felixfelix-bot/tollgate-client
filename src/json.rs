//! Just enough JSON to read the gate's own responses.
//!
//! The gate emits compact `encoding/json` output; the values we need are
//! scalars. A full parser (or a serde dependency) buys nothing here and costs the
//! ability to build for a router, so this stays deliberately small — and it is
//! tested against the exact shapes the module produces.

pub fn field_str(body: &str, key: &str) -> Option<String> {
    let k = format!("\"{key}\"");
    let rest = &body[body.find(&k)? + k.len()..];
    let after = &rest[rest.find('"')? + 1..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

pub fn field_u64(body: &str, key: &str) -> Option<u64> {
    let k = format!("\"{key}\"");
    let mut rest = &body[body.find(&k)? + k.len()..];
    rest = rest.trim_start_matches(|c: char| c == ':' || c == ',' || c.is_whitespace());
    if let Some(stripped) = rest.strip_prefix('"') {
        let digits: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        return digits.parse().ok();
    }
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub fn field_bool(body: &str, key: &str) -> Option<bool> {
    let k = format!("\"{key}\"");
    let mut rest = &body[body.find(&k)? + k.len()..];
    rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_gate_response_shapes() {
        let body = r#"{"status":1,"quote":"q_1","invoice":"lnbc10n1abc","amount":2,"access_granted":false}"#;
        assert_eq!(field_u64(body, "status"), Some(1));
        assert_eq!(field_str(body, "quote").as_deref(), Some("q_1"));
        assert_eq!(field_str(body, "invoice").as_deref(), Some("lnbc10n1abc"));
        assert_eq!(field_u64(body, "amount"), Some(2));
        assert_eq!(field_bool(body, "access_granted"), Some(false));
        assert_eq!(field_str(body, "missing"), None);
    }

    #[test]
    fn a_string_number_is_accepted_where_a_number_is_expected() {
        // Nostr tags quote numbers; JSON does not. Both appear in this protocol.
        assert_eq!(
            field_u64(r#"{"allotment":"60000"}"#, "allotment"),
            Some(60_000)
        );
    }
}
