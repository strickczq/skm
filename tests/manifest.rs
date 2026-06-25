//! `skm.toml` version gate: a missing top-level `version` is a hard error
//! (not a silent default to 1), and only `version = 1` is recognized.

mod support;
use support::*;

#[test]
fn manifest_missing_version_errors() {
    // A missing top-level `version` is a hard error, not a silent default to
    // 1 (an old field-less file must not be misread as v1).
    let env = Env::new();
    env.create_local_skill("x", &[]);
    write_file(
        &env.manifest_path(),
        "[defaults]\nagents = [\"agents\"]\n\n[[skills]]\nname = \"x\"\nlocal = \"./vendor/x\"\n",
        false,
    );
    env.skm()
        .arg("status")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("missing required 'version'"));
}

#[test]
fn manifest_unsupported_version_errors() {
    // In the v1 stage only version = 1 is recognized.
    let env = Env::new();
    env.create_local_skill("x", &[]);
    write_file(
        &env.manifest_path(),
        "version = 2\n[defaults]\nagents = [\"agents\"]\n\n[[skills]]\nname = \"x\"\nlocal = \"./vendor/x\"\n",
        false,
    );
    env.skm()
        .arg("status")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("unsupported manifest version 2"));
}
