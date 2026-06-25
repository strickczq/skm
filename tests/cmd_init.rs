//! `skm init` contract: writes a `version = 1` template, never a lockfile,
//! and refuses to clobber an existing manifest without `--force`.

mod support;
use support::*;

#[test]
fn init_writes_version_1_and_no_lock() {
    // Init: generates a `version = 1` template, does NOT create a lockfile,
    // and hints the user to run sync.
    let env = Env::new();
    env.skm()
        .arg("init")
        .assert()
        .success()
        .stdout(predicates::str::contains("Run 'skm sync'"));

    let toml = env.manifest_text();
    assert!(
        toml.contains("version = 1"),
        "init template must pin version = 1: {toml}"
    );
    // The template must not silently default agents — the user picks
    // their agent(s) explicitly (active line, not the commented example).
    assert!(
        !toml.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("agents") && l.contains('=')
        }),
        "init template must leave [defaults].agents unset: {toml}"
    );
    assert!(!env.lock_path().exists(), "init must not create a lockfile");
}

#[test]
fn init_existing_errors_without_force() {
    // Init: refuses to clobber an existing manifest unless --force.
    let env = Env::new();
    env.skm().arg("init").assert().success();

    env.skm()
        .arg("init")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("already exists"));
}

#[test]
fn init_force_overwrites_existing() {
    let env = Env::new();
    env.skm().arg("init").assert().success();
    env.skm().args(["init", "--force"]).assert().success();
}
