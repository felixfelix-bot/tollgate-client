//! Spec for the scoring + policy engine. Written before the implementation:
//! this file must FAIL to compile until `scoring` and `policy` exist.

use std::collections::HashMap;
use tollgate_client_core::policy::{decide, Caps, Decision};
use tollgate_client_core::scoring::{GatewayHistory, Metric, Offer};

fn ms_offer(id: &str, price_per_step: u64, step_ms: u64) -> Offer {
    Offer {
        gateway_id: id.to_string(),
        mint_url: "https://mint.example".to_string(),
        unit: "sat".to_string(),
        metric: Metric::Milliseconds,
        price_per_step,
        step_size: step_ms,
        min_steps: 1,
    }
}

fn byte_offer(id: &str, price_per_step: u64, step_bytes: u64) -> Offer {
    Offer {
        gateway_id: id.to_string(),
        mint_url: "https://mint.example".to_string(),
        unit: "sat".to_string(),
        metric: Metric::Bytes,
        price_per_step,
        step_size: step_bytes,
        min_steps: 1,
    }
}

fn healthy() -> GatewayHistory {
    let mut h = GatewayHistory::new();
    for _ in 0..10 {
        h.record_success(40.0, 0.0, 20_000.0);
    }
    h
}

fn flaky() -> GatewayHistory {
    let mut h = GatewayHistory::new();
    for _ in 0..4 {
        h.record_success(40.0, 0.0, 20_000.0);
    }
    for _ in 0..6 {
        h.record_failure();
    }
    h
}

fn caps(unit: &str) -> Caps {
    Caps {
        preferred_unit: unit.to_string(),
        max_cost_per_minute: 10.0,
        daily_budget: 1_000.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    }
}

/// €/sat agnostic: pure arithmetic on the advert's own fields.
#[test]
fn cost_per_minute_from_step_math() {
    // 1 sat per 60_000 ms step == 1 sat per minute.
    let o = ms_offer("a", 1, 60_000);
    assert!((o.cost_per_minute().unwrap() - 1.0).abs() < 1e-9);
    // 2 sat per 120_000 ms == 1 sat/min.
    let o = ms_offer("b", 2, 120_000);
    assert!((o.cost_per_minute().unwrap() - 1.0).abs() < 1e-9);
    // A byte offer has no cost-per-minute.
    assert!(byte_offer("c", 1, 1_000_000).cost_per_minute().is_none());
    // 1 sat per 21 MiB step == 1 sat per 21_000_000 bytes.
    let o = byte_offer("d", 1, 21_000_000);
    assert!((o.cost_per_mb().unwrap() - 1.0 / 21.0).abs() < 1e-9);
}

#[test]
fn cheaper_wins_at_equal_quality() {
    let offers = vec![ms_offer("cheap", 1, 60_000), ms_offer("dear", 3, 60_000)];
    let mut h = HashMap::new();
    h.insert("cheap".to_string(), healthy());
    h.insert("dear".to_string(), healthy());
    match decide(&offers, &h, &caps("sat"), None).unwrap() {
        Decision::Buy { gateway_id, .. } => assert_eq!(gateway_id, "cheap"),
        other => panic!("expected Buy, got {other:?}"),
    }
}

/// The whole point of tracking reliability: a slightly dearer gateway that
/// actually works beats a cheap one that fails half the time.
#[test]
fn reliability_beats_a_marginal_price_advantage() {
    let offers = vec![ms_offer("flaky", 1, 60_000), ms_offer("solid", 1, 50_000)];
    let mut h = HashMap::new();
    h.insert("flaky".to_string(), flaky());
    h.insert("solid".to_string(), healthy());
    match decide(&offers, &h, &caps("sat"), None).unwrap() {
        Decision::Buy { gateway_id, .. } => assert_eq!(gateway_id, "solid"),
        other => panic!("expected Buy, got {other:?}"),
    }
}

/// Cross-unit comparison would need an exchange rate the client must never
/// invent. An offer outside the preferred unit is never silently chosen.
#[test]
fn offers_in_another_unit_are_not_comparable() {
    let mut eur = ms_offer("eur-mint", 1, 60_000);
    eur.unit = "eur-cent".to_string();
    let offers = vec![eur];
    let h = HashMap::new();
    match decide(&offers, &h, &caps("sat"), None).unwrap() {
        Decision::Refuse { reason } => assert!(reason.contains("unit"), "reason: {reason}"),
        other => panic!("expected Refuse, got {other:?}"),
    }
}

#[test]
fn a_price_above_the_per_minute_cap_is_never_bought() {
    let offers = vec![ms_offer("pricey", 50, 60_000)];
    let mut h = HashMap::new();
    h.insert("pricey".to_string(), healthy());
    match decide(&offers, &h, &caps("sat"), None).unwrap() {
        Decision::Prompt { reason } => assert!(reason.contains("cap"), "reason: {reason}"),
        other => panic!("expected Prompt, got {other:?}"),
    }
}

#[test]
fn the_daily_budget_is_a_hard_stop() {
    let offers = vec![ms_offer("ok", 1, 60_000)];
    let mut h = HashMap::new();
    h.insert("ok".to_string(), healthy());
    let mut c = caps("sat");
    c.daily_budget = 10.0;
    c.spent_today = 9.5;
    // one step = 1.0 -> would exceed the remaining 0.5
    match decide(&offers, &h, &c, None).unwrap() {
        Decision::Prompt { reason } => assert!(reason.contains("budget"), "reason: {reason}"),
        other => panic!("expected Prompt, got {other:?}"),
    }
}

/// Do not flap between two near-equal gateways.
#[test]
fn hysteresis_keeps_a_good_incumbent() {
    let incumbent = ms_offer("current", 1, 60_000);
    let challenger = ms_offer("challenger", 1, 65_000); // ~8% cheaper per minute
    let mut h = HashMap::new();
    h.insert("current".to_string(), healthy());
    h.insert("challenger".to_string(), healthy());
    match decide(&[incumbent.clone(), challenger.clone()], &h, &caps("sat"), Some("current")).unwrap() {
        Decision::Buy { gateway_id, .. } => assert_eq!(gateway_id, "current"),
        other => panic!("expected Buy(current), got {other:?}"),
    }
    // A clearly better challenger (2x cheaper) does win.
    let mut cheap = ms_offer("challenger", 2, 240_000);
    cheap.gateway_id = "challenger".to_string();
    match decide(&[incumbent, cheap], &h, &caps("sat"), Some("current")).unwrap() {
        Decision::Buy { gateway_id, .. } => assert_eq!(gateway_id, "challenger"),
        other => panic!("expected Buy(challenger), got {other:?}"),
    }
}

/// min_steps from the advert is a purchase floor, not a suggestion.
#[test]
fn respects_the_advertised_minimum_purchase() {
    let mut o = ms_offer("floor", 1, 60_000);
    o.min_steps = 5;
    let mut h = HashMap::new();
    h.insert("floor".to_string(), healthy());
    match decide(&[o], &h, &caps("sat"), None).unwrap() {
        Decision::Buy { steps, .. } => assert!(steps >= 5, "steps: {steps}"),
        other => panic!("expected Buy, got {other:?}"),
    }
}
