//! `skm doctor` diagnostics: healthy after sync, skills_root-not-writable (5),
//! and exec-bit drift reported as read-only drift (2).

mod support;
use support::*;

use std::os::unix::fs::PermissionsExt;

fn local_setup(env: &Env) {
    env.create_local_skill("r", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("r", "./vendor/r"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
}

#[test]
fn doctor_healthy_after_sync() {
    let env = Env::new();
    local_setup(&env);
    env.skm().arg("doctor").assert().success();
}

#[test]
fn doctor_skills_root_not_writable_is_io() {
    // Doctor: skills_root permissions are checked. A non-writable existing
    // root bumps Io (5).
    let env = Env::new();
    local_setup(&env);

    let root = env.agents_root();
    let original = std::fs::metadata(&root).unwrap().permissions();
    let mut ro = original.clone();
    ro.set_mode(0o555);
    std::fs::set_permissions(&root, ro).unwrap();

    // If the effective uid can bypass the mode (root in CI / containers), skip.
    let probe = root.join(".can-still-write");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        let _ = std::fs::set_permissions(&root, original);
        eprintln!("skipping: effective uid bypasses dir mode");
        return;
    }

    let out = env.skm().arg("doctor").output().unwrap();
    // Restore perms before asserting so a failure doesn't poison the tempdir
    // teardown.
    let _ = std::fs::set_permissions(&root, original);
    assert_eq!(
        out.status.code(),
        Some(5),
        "expected exit 5 for unwritable skills_root; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("skills_root not writable"),
        "doctor output missing skills_root probe: {stdout}"
    );
}

#[test]
fn doctor_exec_bit_drift_is_2() {
    // "Most fragile link of the hash model": a git skill includes the
    // exec bit in its hash, so stripping the installed script's +x makes the
    // recomputed hash diverge from the lock. Doctor must report that drift and
    // exit 2 (the read-only "drift observed" code).
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .file("run.sh", "#!/bin/sh\necho hi\n", true) // 100755
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    env.skm().arg("doctor").assert().success(); // healthy before tampering

    // Strip the exec bit on the landed script → corrupt the installed tree.
    let landed = env.agents_root().join("s/run.sh");
    let mut perm = std::fs::metadata(&landed).unwrap().permissions();
    perm.set_mode(0o644);
    std::fs::set_permissions(&landed, perm).unwrap();

    env.skm()
        .arg("doctor")
        .assert()
        .code(2)
        .stdout(predicates::str::contains("drift in 's'"));
}
