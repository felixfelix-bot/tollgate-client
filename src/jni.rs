//! The Android boundary: three `extern "C"` functions and nothing else.
//!
//! Everything the phone does that is not UI lives in this crate, so the JNI layer
//! stays a marshalling shim. Strings cross as JSON: a struct would need an ABI
//! agreement between Kotlin and Rust, and JSON is already the protocol's language.

use std::collections::HashMap;

use jni::objects::{JClass, JString};
use jni::sys::jstring;
use jni::JNIEnv;

use crate::policy::Caps;

fn take(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|v| v.into()).unwrap_or_default()
}

fn give(env: &mut JNIEnv, s: String) -> jstring {
    env.new_string(s)
        .map(|v| v.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn caps_json(caps: &Caps) -> String {
    format!(
        r#"{{"unit":"{}","max_cost_per_minute":{:.3},"daily_budget":{:.3},"spent_today":{:.3}}}"#,
        caps.preferred_unit, caps.max_cost_per_minute, caps.daily_budget, caps.spent_today
    )
}

/// `Core.describe(gateUrl)` — what is this gateway selling, and is it worth it?
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_describe(
    mut env: JNIEnv,
    _class: JClass,
    gate_url: JString,
) -> jstring {
    let url = take(&mut env, &gate_url);
    let caps = Caps {
        preferred_unit: "sat".into(),
        max_cost_per_minute: 2.0,
        daily_budget: 100.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    };
    let out = crate::runner::describe_gate(&url, &caps, &HashMap::new(), 4000)
        .unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e.replace('"', "'")));
    give(&mut env, out)
}

/// `Core.buy(gateUrl, token, attemptId, journalPath)` — spend one note.
///
/// The phone holds no mint and needs no Lightning backend for this: a Cashu note
/// is the payment. Top-up is a token pasted from any wallet, which is the only
/// moment the user has to care.
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_buy(
    mut env: JNIEnv,
    _class: JClass,
    gate_url: JString,
    token: JString,
    attempt_id: JString,
    journal_path: JString,
) -> jstring {
    let url = take(&mut env, &gate_url);
    let token = take(&mut env, &token);
    let attempt_id = take(&mut env, &attempt_id);
    let journal_path = take(&mut env, &journal_path);

    let caps = Caps {
        preferred_unit: "sat".into(),
        max_cost_per_minute: 2.0,
        daily_budget: 100.0,
        spent_today: 0.0,
        switch_margin: 0.20,
    };
    let mut wallet = crate::runner::FileWallet { token };
    let mut journal = match crate::runner::FileJournal::open(&journal_path) {
        Ok(j) => j,
        Err(e) => {
            return give(
                &mut env,
                format!(r#"{{"error":"journal: {}"}}"#, e.replace('"', "'")),
            )
        }
    };
    let out = match crate::runner::run_once(
        &url,
        &caps,
        &HashMap::new(),
        &mut wallet,
        &mut journal,
        &attempt_id,
        8000,
    ) {
        Ok(report) => {
            let result = match report.result {
                Some(crate::agent::AttemptResult::Finished { state, outcome, .. }) => format!(
                    r#"{{"state":"{}","outcome":"{:?}"}}"#,
                    state.as_str(),
                    outcome
                )
                .replace('\n', " "),
                Some(crate::agent::AttemptResult::AlreadyAttempted { state, .. }) => {
                    format!(
                        r#"{{"state":"{}","reason":"already-attempted"}}"#,
                        state.as_str()
                    )
                }
                Some(crate::agent::AttemptResult::NotAttempted { reason }) => {
                    format!(
                        r#"{{"state":"not-attempted","reason":"{}"}}"#,
                        reason.replace('"', "'")
                    )
                }
                None => r#"{"state":"not-attempted"}"#.to_string(),
            };
            format!(
                r#"{{"gate":"{}","decision":"{:?}","result":{}}}"#,
                url,
                report.decision.replace('"', "'"),
                result
            )
        }
        Err(e) => format!(r#"{{"error":"{}"}}"#, e.replace('"', "'")),
    };
    give(&mut env, out)
}

/// `Core.capsFor(unit, maxPerMinute, dailyBudget)` — the caps, as the UI shows them.
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_capsFor(
    mut env: JNIEnv,
    _class: JClass,
    unit: JString,
    max_per_minute: f64,
    daily_budget: f64,
) -> jstring {
    let caps = Caps {
        preferred_unit: take(&mut env, &unit),
        max_cost_per_minute: max_per_minute,
        daily_budget,
        spent_today: 0.0,
        switch_margin: 0.20,
    };
    let out = format!(
        r#"{{"caps":{},"version":"{}"}}"#,
        caps_json(&caps),
        env!("CARGO_PKG_VERSION")
    );
    give(&mut env, out)
}
