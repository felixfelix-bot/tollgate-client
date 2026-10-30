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
