//! Android JNI: discovery, display-only caps, and an ABI-compatible buy refusal.
use crate::policy::Caps;
use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use std::collections::HashMap;

fn take(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|v| v.into()).unwrap_or_default()
}
fn give(env: &mut JNIEnv, s: String) -> jstring {
    env.new_string(s)
        .map(|v| v.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
#[derive(serde::Serialize)]
struct PaymentRefusal {
    state: &'static str,
    code: &'static str,
}
#[derive(serde::Serialize)]
struct ErrorResponse {
    error: String,
}
#[derive(serde::Serialize)]
struct DisplayCaps {
    unit: String,
    max_cost_per_minute: f64,
    daily_budget: f64,
    spent_today: u64,
}
#[derive(serde::Serialize)]
struct CapsResponse {
    caps: DisplayCaps,
    version: &'static str,
}

/// Discovery only. The recommendation does not authorize payment.
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
    let out =
        crate::runner::describe_gate(&url, &caps, &HashMap::new(), 4000).unwrap_or_else(|error| {
            serde_json::to_string(&ErrorResponse { error }).expect("error is serializable")
        });
    give(&mut env, out)
}

/// ABI-compatible tombstone: the symbol stays exported so old binaries load,
/// but it always refuses and never reads arguments, opens storage, or sends.
/// It remains a refusal (not an absent export) in android-hardened builds until a
/// separately reviewed controller is integrated. No FileWallet/run_once fallback.
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_buy(
    mut env: JNIEnv,
    _class: JClass,
    _gate_url: JString,
    _token: JString,
    _attempt_id: JString,
    _journal_path: JString,
) -> jstring {
    give(
        &mut env,
        serde_json::to_string(&PaymentRefusal {
            state: "not-attempted",
            code: "payment-integration-unavailable",
        })
        .expect("fixed refusal is serializable"),
    )
}

/// Legacy display-only caps; never enrolls or authorizes spending policy.
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_capsFor(
    mut env: JNIEnv,
    _class: JClass,
    unit: JString,
    max_per_minute: f64,
    daily_budget: f64,
) -> jstring {
    let out = CapsResponse {
        caps: DisplayCaps {
            unit: take(&mut env, &unit),
            max_cost_per_minute: max_per_minute,
            daily_budget,
            spent_today: 0,
        },
        version: env!("CARGO_PKG_VERSION"),
    };
    give(
        &mut env,
        serde_json::to_string(&out).expect("display caps are serializable"),
    )
}

// ---------------------------------------------------------------------------
// The Lightning lane: the phone buys without holding a wallet.
//
// The token lane needs a Cashu note on the device, which means a wallet, a
// seed, and durable spend state — the parts this build deliberately does not
// have. The Lightning lane needs none of that: the gate creates the invoice,
// the customer pays it in any Lightning wallet, and the gate grants access on
// its own. Two calls, so the UI can show the invoice while it is being paid
// instead of blocking on a synchronous pay step that does not exist here.
// ---------------------------------------------------------------------------

/// `Core.requestInvoice(gateUrl, amountSat, mintUrl, timeoutMs)` — ask the gate
/// to create a quote. Returns `{quote,invoice,amount,mint_url,expiry}` or
/// `{error}`; the BOLT11 is what the customer pays.
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_requestInvoice(
    mut env: JNIEnv,
    _class: JClass,
    gate_url: JString,
    amount_sat: jlong,
    mint_url: JString,
    timeout_ms: jlong,
) -> jstring {
    let url = take(&mut env, &gate_url);
    let mint = take(&mut env, &mint_url);
    let amount = amount_sat.max(1) as u64;
    let timeout = timeout_ms.max(1000) as u64;
    let result = crate::lightning::request_invoice(&url, amount, &mint, timeout);
    give(&mut env, crate::shell_json::request_invoice_json(result))
}

/// `Core.pollInvoice(gateUrl, quote, timeoutMs)` — has the gate granted yet?
/// `access_granted:true` is the ONLY field that means "you are online"; a
/// `paid-but-not-granted` answer is a retry, never a second payment.
#[no_mangle]
pub extern "system" fn Java_dev_tollgate_client_Core_pollInvoice(
    mut env: JNIEnv,
    _class: JClass,
    gate_url: JString,
    quote: JString,
    timeout_ms: jlong,
) -> jstring {
    let url = take(&mut env, &gate_url);
    let quote = take(&mut env, &quote);
    let timeout = timeout_ms.max(1000) as u64;
    let result = crate::lightning::poll_status(&url, &quote, timeout);
    give(&mut env, crate::shell_json::poll_invoice_json(result))
}
