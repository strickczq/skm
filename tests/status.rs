//! Black-box coverage of the five-state table and status exit codes.
//! The existing suite only exercised `drift → 2`; this fills missing/unlocked/
//! installed/extra/foreign, the "unknown (lock missing)" path, multi-agent
//! aggregation, and the unsupported-lock-version error.

mod support;
use support::*;

fn out(env: &Env, args: &[&str]) -> (i32, String) {
    let o = env.skm().args(args).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
    )
}

#[test]
fn installed_on_each_agent_is_exit_0() {
    let env = Env::new();
    env.create_local_skill("a", &[]);
    env.create_local_skill("b", &[]);
    Manifest::v1()
        .default_agents(&["agents", "codex"])
        .skill(Skill::local("a", "./vendor/a"))
        .skill(Skill::local("b", "./vendor/b").agents(&["agents"]))
        .write_to(&env.manifest_path());
    env.skm().args(["sync", "--yes"]).assert().success();

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 0, "all installed → 0; got {s}");
    // `a` lands on both roots, `b` only on agents.
    assert!(env.agents_root().join("a").is_dir() && env.codex_root().join("a").is_dir());
    assert!(env.agents_root().join("b").is_dir() && !env.codex_root().join("b").exists());
    assert!(
        s.contains("installed"),
        "expected installed labels; got {s}"
    );
}

#[test]
fn status_shows_source_line() {
    // Each declared skill reports what it's pinned to, so `status` is a true
    // one-look inventory without opening skm.lock.
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 0, "all installed → 0; got {s}");
    assert!(
        s.contains("source: local ./vendor/a"),
        "expected a source line; got {s}"
    );
    // Nothing to explain when everything is installed → no legend footer.
    assert!(
        !s.contains("legend"),
        "all-installed status must omit the legend; got {s}"
    );
}

#[test]
fn drift_uses_plain_wording_and_legend() {
    // The state string avoids internal jargon ("content_sha256"), and a footer
    // legend explains only the kinds actually shown.
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    write_file(&env.agents_root().join("a/SKILL.md"), "TAMPERED\n", false);

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 2, "drift → 2; got {s}");
    assert!(s.contains("drift (content changed)"), "got {s}");
    assert!(
        !s.contains("content_sha256"),
        "no jargon in status; got {s}"
    );
    assert!(
        s.contains("Legend") && s.contains("drift"),
        "drift legend should appear; got {s}"
    );
    // Only the shown kinds are explained.
    assert!(!s.contains("foreign"), "no foreign here; got {s}");
}

#[test]
fn missing_is_exit_1() {
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    std::fs::remove_dir_all(env.agents_root().join("a")).unwrap();

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 1, "missing → 1");
    assert!(s.contains("missing"), "got {s}");
}

#[test]
fn unlocked_is_exit_1() {
    // Skill in the manifest and on disk, but absent from the (existing) lock.
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success(); // lock has only `a`

    // Add `u` to the manifest and plant its installed dir, but never lock it.
    env.create_local_skill("u", &[]);
    let mut m = env.manifest_text();
    m.push_str("\n[[skills]]\nname=\"u\"\nlocal=\"./vendor/u\"\n");
    write_file(&env.manifest_path(), &m, false);
    write_file(&env.agents_root().join("u/SKILL.md"), "# u\n", false);

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 1, "unlocked aggregates to 1; got {s}");
    assert!(s.contains("unlocked"), "got {s}");
}

#[test]
fn drift_on_one_agent_aggregates_to_2() {
    // Most-severe aggregation — drift(2) wins even when other agents are installed.
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents", "codex"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    write_file(&env.agents_root().join("a/SKILL.md"), "TAMPERED\n", false);
    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 2, "drift wins aggregation; got {s}");
    assert!(s.contains("drift"), "got {s}");
}

#[test]
fn foreign_is_annotated_but_not_counted() {
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    // A directory skm never installed.
    write_file(&env.agents_root().join("stranger/SKILL.md"), "# x\n", false);

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 0, "foreign must not affect the exit code; got {s}");
    assert!(
        s.contains("foreign"),
        "foreign should be annotated; got {s}"
    );
}

#[test]
fn managed_extra_is_listed() {
    let env = Env::new();
    env.create_local_skill("m", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("m", "./vendor/m"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    env.skm()
        .args(["remove", "m", "--no-sync"])
        .assert()
        .success();

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 0, "extra does not affect exit code; got {s}");
    assert!(s.contains("extra (managed"), "got {s}");
}

#[test]
fn lock_missing_reports_unknown_exit_1() {
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    std::fs::remove_file(env.lock_path()).unwrap();

    let (code, s) = out(&env, &["status"]);
    assert_eq!(code, 1, "lock missing → 1");
    assert!(s.contains("unknown (lock missing)"), "got {s}");
    assert!(s.contains("ownership unavailable"), "got {s}");
}

#[test]
fn unsupported_lock_version_errors() {
    // An unrecognized lock version aborts with the documented hint.
    let env = Env::new();
    env.create_local_skill("a", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("a", "./vendor/a"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    let lock = env.lock_text().replacen("version = 1", "version = 2", 1);
    write_file(&env.lock_path(), &lock, false);

    env.skm()
        .arg("status")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("unsupported lock version 2"));
}
