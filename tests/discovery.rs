//! Spec for parsing a TollGate v1 advertisement (kind:10021).
//!
//! Written before the implementation: this must fail to compile until
//! `discovery` exists. The grammar is the one `tollgate-module-basic-go`
//! actually emits (`CreateAdvertisement`), with the stricter money rules a
//! paying client owes itself — an advert that cannot be priced is rejected, and
//! an offer that cannot be paid is dropped.

use tollgate_client_core::discovery::{parse_advertisement, ParseError};
use tollgate_client_core::scoring::Metric;

fn tags(rows: Vec<Vec<&str>>) -> Vec<Vec<String>> {
    rows.into_iter()
        .map(|r| r.into_iter().map(|s| s.to_string()).collect())
        .collect()
}

fn valid_tags() -> Vec<Vec<String>> {
    tags(vec![
        vec!["metric", "milliseconds"],
        vec!["step_size", "60000"],
        vec!["tips", "1", "2"],
        vec![
            "price_per_step",
            "cashu",
            "2",
            "sat",
            "https://mint.example",
            "1",
        ],
    ])
}

#[test]
fn parses_the_module_own_advert() {
    let ad = parse_advertisement(10021, &valid_tags()).expect("valid advert");
    assert_eq!(ad.metric, Metric::Milliseconds);
    assert_eq!(ad.step_size, 60_000);
    assert_eq!(ad.offers.len(), 1);
    let o = &ad.offers[0];
    assert_eq!(o.gateway_id, "");
    assert_eq!(o.unit, "sat");
    assert_eq!(o.price_per_step, 2);
    assert_eq!(o.min_steps, 1);
    // 2 sat per 60 s step == 2 sat per minute
    assert!((o.cost_per_minute().unwrap() - 2.0).abs() < 1e-9);
}

#[test]
fn rejects_a_foreign_event_kind() {
    assert!(matches!(
        parse_advertisement(1022, &valid_tags()),
        Err(ParseError::WrongKind(1022))
    ));
}

/// A zero or missing step size means the price cannot be turned into a rate.
/// Reject the advert; do not silently price it as zero.
#[test]
fn rejects_an_advert_without_a_usable_step_size() {
    let mut t = valid_tags();
    t.retain(|r| r[0] != "step_size");
    assert!(matches!(
        parse_advertisement(10021, &t),
        Err(ParseError::MissingStepSize)
    ));

    let mut t = valid_tags();
    for r in t.iter_mut() {
        if r[0] == "step_size" {
            r[1] = "0".to_string();
        }
    }
    assert!(matches!(
        parse_advertisement(10021, &t),
        Err(ParseError::MissingStepSize)
    ));
}

#[test]
fn rejects_an_unknown_metric() {
    let mut t = valid_tags();
    for r in t.iter_mut() {
        if r[0] == "metric" {
            r[1] = "furlongs".to_string();
        }
    }
    assert!(matches!(
        parse_advertisement(10021, &t),
        Err(ParseError::UnknownMetric(_))
    ));
}

/// Only `cashu` is payable by this client. A lightning-priced offer is not an
/// error — it is simply not something we can buy, so it is dropped.
#[test]
fn drops_offers_that_are_not_cashu() {
    let mut t = valid_tags();
    t.push(
        tags(vec![vec![
            "price_per_step",
            "lightning",
            "1",
            "sat",
            "https://ln.example",
            "1",
        ]])
        .pop()
        .unwrap(),
    );
    let ad = parse_advertisement(10021, &t).unwrap();
    assert_eq!(ad.offers.len(), 1);
    assert_eq!(ad.offers[0].price_per_step, 2);
}

#[test]
fn drops_unpayable_offers() {
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "price_per_step",
            "cashu",
            "0",
            "sat",
            "https://mint.example",
            "1",
        ], // free
        vec![
            "price_per_step",
            "cashu",
            "2",
            "",
            "https://mint.example",
            "1",
        ], // no unit
        vec!["price_per_step", "cashu", "2", "sat", "", "1"], // no mint
        vec![
            "price_per_step",
            "cashu",
            "x",
            "sat",
            "https://mint.example",
            "1",
        ], // not a number
        vec![
            "price_per_step",
            "cashu",
            "2",
            "sat",
            "https://mint.example",
        ], // too short
    ];
    for c in cases {
        let mut t = valid_tags();
        t.push(tags(vec![c.clone()]).pop().unwrap());
        let ad = parse_advertisement(10021, &t).unwrap();
        assert_eq!(ad.offers.len(), 1, "accepted an unpayable offer: {c:?}");
    }
}

#[test]
fn missing_min_steps_defaults_to_one_and_sats_is_normalised() {
    let t = tags(vec![
        vec!["metric", "bytes"],
        vec!["step_size", "22020096"],
        vec![
            "price_per_step",
            "cashu",
            "1",
            "sats",
            "https://mint.example",
            "x",
        ],
    ]);
    let ad = parse_advertisement(10021, &t).unwrap();
    assert_eq!(ad.metric, Metric::Bytes);
    assert_eq!(ad.offers[0].unit, "sat");
    assert_eq!(ad.offers[0].min_steps, 1);
}

/// Two identical tags are one offer, not two — otherwise the ranking sees the
/// same gateway twice and the budget maths double-counts it.
#[test]
fn deduplicates_identical_offers() {
    let mut t = valid_tags();
    t.push(
        tags(vec![vec![
            "price_per_step",
            "cashu",
            "2",
            "sat",
            "https://mint.example",
            "1",
        ]])
        .pop()
        .unwrap(),
    );
    let ad = parse_advertisement(10021, &t).unwrap();
    assert_eq!(ad.offers.len(), 1);
}

/// An advert with no payable offer is still a valid advert (we may know the
/// gateway but not be able to buy from it). The caller decides; the parser does
/// not invent an offer.
#[test]
fn an_advert_with_no_payable_offer_is_valid_and_empty() {
    let t = tags(vec![
        vec!["metric", "milliseconds"],
        vec!["step_size", "60000"],
    ]);
    let ad = parse_advertisement(10021, &t).unwrap();
    assert!(ad.offers.is_empty());
    assert_eq!(ad.step_size, 60_000);
}
