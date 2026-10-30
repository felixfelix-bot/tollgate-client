//! Buy / don't buy, from whom, how much.
//!
//! Caps are hard gates evaluated BEFORE value: a cheap gateway that busts the
//! daily budget or the per-minute price cap is never chosen silently.

use crate::scoring::{GatewayHistory, Offer};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Caps {
    /// The only unit the client will spend. Offers in another unit are not
    /// comparable and are never silently substituted.
    pub preferred_unit: String,
    /// Ceiling on the normalised cost (per minute for time, per MB for bytes),
    /// in `preferred_unit`.
    pub max_cost_per_minute: f64,
    pub daily_budget: f64,
    pub spent_today: f64,
    /// Only switch away from the incumbent for a strictly better offer by this
    /// fraction — stops the client flapping between near-equal gateways.
    pub switch_margin: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Buy {
        gateway_id: String,
        steps: u64,
        cost: f64,
        unit: String,
    },
    /// A human must say yes (over a cap / over budget).
    Prompt { reason: String },
    /// Not an option at all (nothing comparable, nothing to buy).
    Refuse { reason: String },
}

pub fn decide(
    offers: &[Offer],
    histories: &HashMap<String, GatewayHistory>,
    caps: &Caps,
    incumbent: Option<&str>,
) -> Result<Decision, String> {
    if offers.is_empty() {
        return Ok(Decision::Refuse {
            reason: "no tollgate offers are currently advertised".into(),
        });
    }

    let mut comparable: Vec<(f64, &Offer)> = offers
        .iter()
        .filter(|o| o.unit == caps.preferred_unit)
        .filter_map(|o| {
            let cost = o.normalized_cost()?;
            let history = histories.get(&o.gateway_id).cloned().unwrap_or_default();
            Some((history.value(cost), o))
        })
        .collect();

    if comparable.is_empty() {
        let units: Vec<&str> = {
            let mut u: Vec<&str> = offers.iter().map(|o| o.unit.as_str()).collect();
            u.sort_unstable();
            u.dedup();
            u
        };
        return Ok(Decision::Refuse {
            reason: format!(
                "no offer in the preferred unit {:?} (advertised: {:?}); refusing to compare across units without an exchange rate",
                caps.preferred_unit, units
            ),
        });
    }

    comparable.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let best_value = comparable[0].0;
    let mut chosen = comparable[0].1;

    if let Some(inc_id) = incumbent {
        if chosen.gateway_id != inc_id {
            if let Some((inc_value, inc_offer)) =
                comparable.iter().find(|(_, o)| o.gateway_id == inc_id).copied()
            {
                if best_value <= inc_value * (1.0 + caps.switch_margin) {
                    chosen = inc_offer;
                }
            }
        }
    }

    let cost = chosen
        .normalized_cost()
        .ok_or_else(|| format!("offer {} has no normalised cost", chosen.gateway_id))?;

    if cost > caps.max_cost_per_minute {
        return Ok(Decision::Prompt {
            reason: format!(
                "{} costs {:.4} {}/{} which is above the per-minute cap {:.4}",
                chosen.gateway_id,
                cost,
                caps.preferred_unit,
                unit_label(chosen),
                caps.max_cost_per_minute
            ),
        });
    }

    let spend = chosen.total_cost();
    let remaining = caps.daily_budget - caps.spent_today;
    if spend > remaining {
        return Ok(Decision::Prompt {
            reason: format!(
                "buying {} step(s) from {} costs {:.4} {} but only {:.4} of the daily budget remains",
                chosen.steps_to_buy(),
                chosen.gateway_id,
                spend,
                chosen.unit,
                remaining.max(0.0)
            ),
        });
    }

    Ok(Decision::Buy {
        gateway_id: chosen.gateway_id.clone(),
        steps: chosen.steps_to_buy(),
        cost: spend,
        unit: chosen.unit.clone(),
    })
}

fn unit_label(offer: &Offer) -> &'static str {
    match offer.metric {
        crate::scoring::Metric::Milliseconds => "minute",
        crate::scoring::Metric::Bytes => "MB",
    }
}
