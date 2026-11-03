//! The Rust-side mint resolution, pinned without a JVM.
//!
//! The Java shell used to scrape `describe`'s JSON by hand to find the mint,
//! and read an empty `mints` array as the literal string "decision" — the
//! substring scan ran past `]` into the next key. These tests pin the two
//! pieces that replaced it: the advert→mint-list step (done in Rust, on the
//! advert the core itself parsed) and the JSON the shell receives.

use tollgate_client_core::mint_choice::choose_mint;
use tollgate_client_core::shell_json::resolve_mint_json;

#[test]
fn an_empty_advertised_list_is_an_error_not_a_bare_word() {
    // The exact shape that produced "decision": no offers, so no mints.
    let err = choose_mint("", &[]).unwrap_err();
    assert!(err.contains("no mint"), "{err}");

    let json = resolve_mint_json(Err(err));
    assert!(json.starts_with(r#"{"error":"#), "{json}");
    assert!(
        !json.contains("mint\":\""),
        "no mint may be offered: {json}"
    );
}

#[test]
fn a_resolved_mint_is_reported_as_the_gate_spells_it() {
    let advertised = vec![
        "https://mint.one/".to_string(),
        "https://mint.two/".to_string(),
    ];
    let chosen = choose_mint("https://mint.two", &advertised).unwrap();
    let json = resolve_mint_json(Ok(chosen));
    assert_eq!(json, r#"{"mint":"https://mint.two/"}"#);
}

#[test]
fn an_override_the_gate_does_not_advertise_is_refused_with_both_named() {
    let advertised = vec!["https://mint.one/".to_string()];
    let err = choose_mint("https://stale-lab-mint/", &advertised).unwrap_err();
    // A leftover lab value must not reach requestInvoice: the gate would refuse
    // it there, and the user would see a raw refusal instead of this reason.
    assert!(err.contains("stale-lab-mint"), "{err}");
    assert!(err.contains("mint.one"), "{err}");
    let json = resolve_mint_json(Err(err));
    assert!(json.contains("does not accept"), "{json}");
}

#[test]
fn a_mint_url_containing_quotes_cannot_break_the_json() {
    let json = resolve_mint_json(Ok("https://evil/\"><script>".to_string()));
    assert!(json.starts_with(r#"{"mint":"#), "{json}");
    assert!(json.contains(r#"\""#), "quote must be escaped: {json}");
    // Exactly one key: the injected `"` did not open a second field.
    assert_eq!(json.matches(r#"mint"#).count(), 1, "{json}");
}

#[test]
fn two_spellings_of_one_mint_do_not_yield_two_choices() {
    // Dedupe happens on mint identity, so the trailing-space spelling can never
    // be the entry a shell picks first.
    use tollgate_client_core::mint_choice::same_mint;
    assert!(same_mint("https://mint.one/", "https://mint.one/ "));
    assert!(same_mint("HTTPS://MINT.ONE", "https://mint.one/"));
    assert!(!same_mint("https://mint.one/a", "https://mint.one/b"));
}
