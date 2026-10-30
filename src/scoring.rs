//! Reliability, quality and price → one comparable value.
//!
//! Rules that are deliberate, not incidental:
//! - Price is always normalised from the advert's OWN fields (`price_per_step`,
//!   `step_size`) into the metric's canonical unit (minute for time, MB for
//!   bytes). No exchange rate is ever invented here.
//! - Success probability is Beta(1,1)-smoothed, so one bad purchase does not
//!   blacklist a gateway and one good one does not crown it.
//! - Quality is a blend of RTT, throughput and loss against fixed targets;
//!   an unmeasured gateway scores neutral (0.5), never optimistic.

pub const TARGET_RTT_MS: f64 = 150.0;
pub const TARGET_KBPS: f64 = 5_000.0;
const EWMA_ALPHA: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Milliseconds,
    Bytes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub gateway_id: String,
    pub unit: String,
    pub metric: Metric,
    pub price_per_step: u64,
    pub step_size: u64,
    pub min_steps: u64,
}

impl Offer {
    /// Cost of one minute of access, in the advert's unit. `None` for a
    /// non-time metric — the two metrics are not interconvertible without a
    /// link model, so callers must not fold them together.
    pub fn cost_per_minute(&self) -> Option<f64> {
        match self.metric {
            Metric::Milliseconds if self.step_size > 0 => {
                Some(self.price_per_step as f64 * 60_000.0 / self.step_size as f64)
            }
            _ => None,
        }
    }

    /// Cost of one megabyte (decimal MB, matching how a data plan is quoted).
    pub fn cost_per_mb(&self) -> Option<f64> {
        match self.metric {
            Metric::Bytes if self.step_size > 0 => {
                Some(self.price_per_step as f64 * 1_000_000.0 / self.step_size as f64)
            }
            _ => None,
        }
    }

    /// Cost in the metric's own canonical unit (minute, or MB).
    pub fn normalized_cost(&self) -> Option<f64> {
        self.cost_per_minute().or_else(|| self.cost_per_mb())
    }

    pub fn steps_to_buy(&self) -> u64 {
        self.min_steps.max(1)
    }

    pub fn total_cost(&self) -> f64 {
        self.price_per_step as f64 * self.steps_to_buy() as f64
    }
}

#[derive(Debug, Clone, Default)]
pub struct GatewayHistory {
    pub successes: u32,
    pub failures: u32,
    pub samples: u32,
    pub rtt_ms: f64,
    pub loss: f64,
    pub kbps: f64,
}

impl GatewayHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_success(&mut self, rtt_ms: f64, loss: f64, kbps: f64) {
        self.successes += 1;
        self.samples += 1;
        if self.samples == 1 {
            self.rtt_ms = rtt_ms;
            self.loss = loss;
            self.kbps = kbps;
        } else {
            self.rtt_ms = EWMA_ALPHA * rtt_ms + (1.0 - EWMA_ALPHA) * self.rtt_ms;
            self.loss = EWMA_ALPHA * loss + (1.0 - EWMA_ALPHA) * self.loss;
            self.kbps = EWMA_ALPHA * kbps + (1.0 - EWMA_ALPHA) * self.kbps;
        }
    }

    pub fn record_failure(&mut self) {
        self.failures += 1;
    }

    /// Beta(1,1) posterior mean: an unobserved gateway sits at 0.5.
    pub fn success_probability(&self) -> f64 {
        (self.successes as f64 + 1.0) / (self.successes as f64 + self.failures as f64 + 2.0)
    }

    pub fn quality(&self) -> f64 {
        if self.samples == 0 {
            return 0.5;
        }
        let latency = 1.0 / (1.0 + self.rtt_ms / TARGET_RTT_MS);
        let throughput = (self.kbps / TARGET_KBPS).clamp(0.0, 1.0);
        let delivery = 1.0 - self.loss.clamp(0.0, 1.0);
        (0.5 * latency + 0.3 * throughput + 0.2 * delivery).clamp(0.0, 1.0)
    }

    /// Value per unit cost. Bigger is better; the absolute number is meaningless
    /// across units or metrics, which is why ranking is unit-scoped.
    pub fn value(&self, normalized_cost: f64) -> f64 {
        if normalized_cost <= 0.0 {
            return f64::INFINITY;
        }
        self.success_probability() * self.quality() / normalized_cost
    }
}
