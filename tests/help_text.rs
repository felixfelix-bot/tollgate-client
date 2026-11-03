//! The shipped binary must describe the lanes it actually has.
//!
//! `--lightning` was implemented but missing from the usage string, so a user
//! reading `--help` concluded the packaged agent only spoke ecash — the one
//! lane the QEMU/module interop run verified is the one the help text omitted.
//! A help string is the contract a user reads before spending money, so it is
//! asserted here rather than left to eyeballing.

use std::process::Command;

fn help_text() -> String {
    let bin = env!("CARGO_BIN_EXE_tollgate-agent");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(
        out.status.success(),
        "--help must exit 0, got {:?}",
        out.status
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn help_documents_the_lightning_lane() {
    let help = help_text();
    assert!(
        help.contains("--lightning"),
        "--help omits --lightning, but the binary accepts it and the packaged \
         service can also select it with TOLLGATE_MODE=lightning; usage was:\n{help}"
    );
}

#[test]
fn help_documents_the_mode_env_var() {
    let help = help_text();
    assert!(
        help.contains("TOLLGATE_MODE"),
        "--help omits TOLLGATE_MODE, which the packaged service uses to select \
         the lightning lane without a token file; usage was:\n{help}"
    );
}
