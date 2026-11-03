//! When to top up an existing session.
//!
//! The gate tops a session up rather than replacing it: paying while `active`
//! adds the new allotment to what is left (measured against
//! `tollgate-module-basic-go`: `/usage` went `0/22020096` → `0/44040192` after a
//! second 1-sat purchase). So "renew before it runs out" is a real operation,
//! not a re-purchase, and the client's only job is to decide *when*.
//!
//! This module is the decision and nothing else — no network, no money, no
//! clock. The shell supplies a reading and a policy; the answer is one of
//! `TopUp`, `Wait`, or `Blocked`. Keeping it pure is what makes "do not buy"
//! testable, and "do not buy" is the half that protects the wallet.
//!
//! Two invariants worth stating, because each is a way to lose money:
//!
//! 1. **One top-up in flight at a time.** `policy.pending` is set while a
//!    purchase has an unknown outcome; a second purchase then would pay twice
//!    for one extension. The answer is `Wait`, never `TopUp`.
//! 2. **The budget binds before value does.** A session that is nearly out and
//!    a price that is good is still a refusal once the daily cap is reached.
//!    `Blocked` is not a failure, it is the cap doing its job.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Never had a session. A first purchase, not a top-up.
    None,
    /// Allotment left.
    Active,
    /// Had a session; it is used up.
    Expired,
}

impl SessionState {
    pub fn parse(raw: &str) -> Option<SessionState> {
        match raw.trim().trim_matches('"') {
            "none" => Some(SessionState::None),
            "active" => Some(SessionState::Active),
            "expired" => Some(SessionState::Expired),
            _ => None,
        }
    }
}

/// What the gate said about this client, from `/session-state` + `/usage`.
///
/// `used`/`total` come from `/usage` (`"<used>/<total>"`). The gate answers
/// `-1/-1` for a client it cannot meter, which is not zero usage — it is
/// "unknown", and `total <= 0` carries that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionReading {
    pub state: SessionState,
    pub used: i64,
    pub total: i64,
}

impl SessionReading {
    /// The gate's own caveat: an unmeterable client reads `-1/-1`.
    pub fn is_meterable(&self) -> bool {
        self.total > 0 && self.used >= 0
    }

    /// Bytes (or milliseconds) left, floored at zero.
    pub fn remaining(&self) -> i64 {
        if !self.is_meterable() {
            return 0;
        }
        (self.total - self.used).max(0)
    }

    pub fn remaining_fraction(&self) -> f64 {
        if !self.is_meterable() {
            return 0.0;
        }
        self.remaining() as f64 / self.total as f64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenewalPolicy {
    /// Write it down explicitly: though 1.0 reads fine, a policy struct should
    /// be constructible by a reader who has not memorised the module.
    // Defaults live in `RenewalPolicy::default_new()`.
    pub top_up_below: f64,
    /// A top-up smaller than this is not worth a payment.
    pub min_top_up: u64,
    /// Daily spend cap, in the advert's unit (sats today).
    pub daily_budget: u64,
    pub spent_today: u64,
    /// Cost of the proposed top-up.
    pub cost: u64,
    /// A purchase is in flight or its outcome is unknown. Never start another.
    pub pending: bool,
}

impl RenewalPolicy {
    /// 20 % headroom, which at the module's default ~22 MB step is ~4.4 MB of
    /// runway — enough to cover one 60 s tick plus a retry without the customer
    /// seeing a gap.
    pub fn default_new() -> Self {
        RenewalPolicy {
            top_up_below: 0.20,
            min_top_up: 1,
            daily_budget: 0,
            spent_today: 0,
            cost: 0,
            pending: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewalDecision {
    /// Buy now; `why` is what to tell the user.
    TopUp { why: String },
    /// Nothing to do yet. `remaining_fraction` lets the UI show a countdown.
    Wait { remaining_fraction_milli: u32 },
    /// Do not buy, and say why. Never silent: a silent refusal looks like a bug.
    Blocked { why: String },
}

pub fn decide(reading: SessionReading, policy: &RenewalPolicy) -> RenewalDecision {
    // 1. Never two purchases for one extension.
    if policy.pending {
        return RenewalDecision::Blocked {
            why: "a top-up is already in flight or its outcome is unknown; \
                  settling that first is what stops a double payment"
                .into(),
        };
    }

    // 2. The cap binds before anything else, including "the session is about to
    //    die". Running out of data is annoying; overspending a daily budget is
    //    not a thing the customer agreed to.
    if policy.daily_budget > 0
        && policy.spent_today.saturating_add(policy.cost) > policy.daily_budget
    {
        return RenewalDecision::Blocked {
            why: format!(
                "today's spend {} plus this top-up {} would pass the daily budget {}",
                policy.spent_today, policy.cost, policy.daily_budget
            ),
        };
    }

    // 3. The gate cannot meter this client. Its usage is "unknown", so there is
    //    no honest threshold to apply — topping up blind could extend a session
    //    that has barely been used, or one that no longer exists.
    if reading.state == SessionState::Active && !reading.is_meterable() {
        return RenewalDecision::Blocked {
            why: "the gate reports this client as unmeterable (-1/-1); \
                  there is no remaining amount to renew against"
                .into(),
        };
    }

    match reading.state {
        // No session yet: this is the first purchase, not a top-up. Say so, so
        // a shell does not label a first buy "renewal".
        SessionState::None => RenewalDecision::TopUp {
            why: "no session yet".into(),
        },
        SessionState::Expired => RenewalDecision::TopUp {
            why: "the session has run out".into(),
        },
        SessionState::Active => {
            let frac = reading.remaining_fraction();
            if frac > policy.top_up_below {
                return RenewalDecision::Wait {
                    remaining_fraction_milli: (frac * 1000.0).round() as u32,
                };
            }
            // Below the threshold: check the top-up is worth paying for.
            let remaining = reading.remaining() as u64;
            if policy.cost > 0 && policy.cost < policy.min_top_up {
                return RenewalDecision::Blocked {
                    why: format!(
                        "a {} top-up is below the {} minimum worth paying for",
                        policy.cost, policy.min_top_up
                    ),
                };
            }
            RenewalDecision::TopUp {
                why: format!(
                    "{} of {} left ({:.0}%), at or below the {:.0}% threshold",
                    remaining,
                    reading.total,
                    frac * 100.0,
                    policy.top_up_below * 100.0
                ),
            }
        }
    }
}

/// Parse the gate's `/usage` body: `"<used>/<total>"`, or `-1/-1` when it
/// cannot meter the client.
pub fn parse_usage(body: &str) -> Option<(i64, i64)> {
    let t = body.trim().trim_matches('"');
    let (used, total) = t.split_once('/')?;
    Some((used.trim().parse().ok()?, total.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RenewalPolicy {
        let mut p = RenewalPolicy::default_new();
        p.daily_budget = 100;
        p.spent_today = 0;
        p.cost = 1;
        p
    }

    fn active(used: i64, total: i64) -> SessionReading {
        SessionReading {
            state: SessionState::Active,
            used,
            total,
        }
    }

    #[test]
    fn a_healthy_session_is_left_alone() {
        // 50% left, threshold 20%: doing nothing is the correct action, and it
        // must be a Wait rather than a TopUp or the app would spend every tick.
        match decide(active(50, 100), &policy()) {
            RenewalDecision::Wait {
                remaining_fraction_milli,
            } => {
                assert_eq!(remaining_fraction_milli, 500)
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn a_session_below_the_threshold_is_topped_up() {
        match decide(active(85, 100), &policy()) {
            RenewalDecision::TopUp { why } => assert!(why.contains("15"), "{why}"),
            other => panic!("expected TopUp, got {other:?}"),
        }
    }

    #[test]
    fn exactly_at_the_threshold_counts_as_running_out() {
        // Boundary: 80/100 is exactly 20% left. Topping up here (not at 19%)
        // is what gives the customer runway before the gate cuts them off.
        assert!(matches!(
            decide(active(80, 100), &policy()),
            RenewalDecision::TopUp { .. }
        ));
    }

    #[test]
    fn a_purchase_already_in_flight_blocks_a_second_one() {
        let mut p = policy();
        p.pending = true;
        // The session is nearly dead — the tempting case — and still must not
        // fire: an unknown outcome plus a new purchase is how one extension is
        // paid for twice.
        assert!(matches!(
            decide(active(99, 100), &p),
            RenewalDecision::Blocked { .. }
        ));
    }

    #[test]
    fn the_daily_budget_beats_a_dying_session() {
        let mut p = policy();
        p.spent_today = 100;
        p.cost = 1;
        match decide(active(99, 100), &p) {
            RenewalDecision::Blocked { why } => assert!(why.contains("daily budget"), "{why}"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn an_expired_session_is_topped_up_even_with_zero_remaining() {
        // total > 0 but fully used: remaining is 0, not "unknown".
        let r = SessionReading {
            state: SessionState::Expired,
            used: 100,
            total: 100,
        };
        assert!(matches!(
            decide(r, &policy()),
            RenewalDecision::TopUp { .. }
        ));
    }

    #[test]
    fn an_unmeterable_active_session_is_not_topped_up_blind() {
        // The gate's -1/-1. There is no remaining fraction to act on; topping up
        // here would be spending against a number that does not exist.
        let r = active(-1, -1);
        assert!(!r.is_meterable());
        assert!(matches!(
            decide(r, &policy()),
            RenewalDecision::Blocked { .. }
        ));
    }

    #[test]
    fn no_session_yet_is_a_first_purchase() {
        let r = SessionReading {
            state: SessionState::None,
            used: 0,
            total: 0,
        };
        match decide(r, &policy()) {
            RenewalDecision::TopUp { why } => assert!(why.contains("no session")),
            other => panic!("expected TopUp, got {other:?}"),
        }
    }

    #[test]
    fn a_top_up_that_is_not_worth_paying_for_is_refused() {
        let mut p = policy();
        p.min_top_up = 10;
        p.cost = 1;
        match decide(active(95, 100), &p) {
            RenewalDecision::Blocked { why } => assert!(why.contains("minimum"), "{why}"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn usage_parsing_handles_the_gates_two_shapes() {
        assert_eq!(parse_usage("0/22020096"), Some((0, 22020096)));
        assert_eq!(parse_usage("1024/4096"), Some((1024, 4096)));
        assert_eq!(parse_usage("-1/-1"), Some((-1, -1)));
        assert_eq!(parse_usage("garbage"), None);
        assert_eq!(parse_usage(""), None);
    }

    #[test]
    fn session_state_parsing_is_strict_about_unknown_words() {
        assert_eq!(SessionState::parse("active"), Some(SessionState::Active));
        assert_eq!(
            SessionState::parse(" expired "),
            Some(SessionState::Expired)
        );
        assert_eq!(SessionState::parse("\"none\""), Some(SessionState::None));
        // An unknown word must not silently become "None" (a free purchase).
        assert_eq!(SessionState::parse("active-ish"), None);
    }
}
