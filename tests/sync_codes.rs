//! `skm sync` exit-code table: drift (2), frozen abort (3), network/offline (4),
//! lock-missing (6), and dry-run prediction (10) across the
//! --frozen/--locked/--offline/--dry-run flag matrix.

mod support;
use support::*;

fn setup_local(env: &Env) {
    env.create_local_skill("rev", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());
}

#[test]
fn status_drift_is_2_frozen_abort_is_3_lock_missing_is_6() {
    let env = Env::new();
    setup_local(&env);
    env.skm().arg("sync").assert().success();

    // Drift the installed dir → status reports 2.
    write_file(&env.agents_root().join("rev/SKILL.md"), "tampered\n", false);
    env.assert_drift();

    // Also drift the source → frozen materialization differs from lock → 3.
    write_file(
        &env.project.join("vendor/rev/SKILL.md"),
        "source-changed\n",
        false,
    );
    env.skm().args(["sync", "--frozen"]).assert().code(3);

    // Lock missing under --frozen → 6.
    std::fs::remove_file(env.lock_path()).unwrap();
    env.skm().args(["sync", "--frozen"]).assert().code(6);
}

#[test]
fn dry_run_reports_changes_with_code_10() {
    let env = Env::new();
    setup_local(&env);
    // Nothing installed yet → dry-run shows the install section and exits 10.
    env.skm()
        .args(["sync", "--dry-run"])
        .assert()
        .code(10)
        .stdout(predicates::str::contains("Install:"));
    // dry-run must not have written the lock or deployed.
    assert!(!env.lock_path().exists());
    assert!(!env.agents_root().join("rev").exists());

    // After a real sync, dry-run is a no-op → code 0.
    env.skm().arg("sync").assert().success();
    env.skm().args(["sync", "--dry-run"]).assert().code(0);
}

#[test]
fn frozen_dry_run_predicts_drift_with_code_3() {
    // --dry-run "needed for resolve and hashing": --frozen --dry-run must
    // materialize and predict a real --frozen abort instead of reporting NOOP.
    let env = Env::new();
    setup_local(&env);
    env.skm().arg("sync").assert().success();

    // Mutate the local source so materialization drifts from the lock.
    write_file(
        &env.project.join("vendor/rev/SKILL.md"),
        "source-changed\n",
        false,
    );

    let out = env
        .skm()
        .args(["sync", "--frozen", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "frozen --dry-run should predict the abort with code 3; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Frozen drift"),
        "expected Frozen drift in plan output; got {stdout}"
    );

    // Dry-run never writes to disk.
    assert!(env.lock_path().exists());
    // Real --frozen confirms the prediction.
    env.skm().args(["sync", "--frozen"]).assert().code(3);
}

#[test]
fn offline_cache_miss_is_4_but_frozen_is_6() {
    // `--offline` table: cache miss is generally 4, but under --frozen the
    // missing artifact required by the lock is 6.
    let env = Env::new();
    let zip = Archive::zip().file("SKILL.md", "# z\n", false).build();
    let mut server = mockito::Server::new();
    server.mock("GET", "/a.zip").with_body(zip).create();
    let url = format!("{}/a.zip", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::zip("z", &url))
        .write_to(&env.manifest_path());

    // Populate cache + lock.
    env.skm().arg("sync").assert().success();
    // Wipe the cache so the artifact is no longer locally available.
    env.skm().args(["cache", "clean"]).assert().success();
    // Also wipe the installed dir so deploy actually has to materialize.
    std::fs::remove_dir_all(env.agents_root().join("z")).unwrap();

    // --frozen --offline → must report LockMissing (6).
    env.skm()
        .args(["sync", "--frozen", "--offline"])
        .assert()
        .code(6);

    // Plain --offline (no --frozen) → must report Network (4).
    env.skm().args(["sync", "--offline"]).assert().code(4);
}

#[test]
fn offline_dry_run_defers_unresolvable_source_with_code_10() {
    // --offline + --dry-run: a source that needs the network to (re)resolve is
    // flagged "may update — offline, source identity changed" instead of erroring;
    // exit 10, and the lock/skills_root hard boundary is respected.
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# g\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("g", &repo.url()))
        .write_to(&env.manifest_path());

    // No lock yet + offline → git cannot resolve, but dry-run must not error: it
    // reports the entry as "may update" and exits 10.
    env.skm()
        .args(["sync", "--dry-run", "--offline"])
        .assert()
        .code(10)
        .stdout(predicates::str::contains(
            "offline, source identity changed",
        ));

    // Hard boundary: dry-run wrote no lock and deployed nothing.
    assert!(!env.lock_path().exists());
    env.assert_not_installed("g");

    // Sanity: a real online sync still resolves and installs normally.
    env.skm().arg("sync").assert().success();
    env.assert_installed("g");
}

#[test]
fn locked_mismatch_is_3() {
    let env = Env::new();
    setup_local(&env);
    env.skm().arg("sync").assert().success();
    // Add a new skill to the manifest without locking.
    let src = env.project.join("vendor/two");
    write_file(&src.join("SKILL.md"), "# two\n", false);
    let mut m = env.manifest_text();
    m.push_str("\n[[skills]]\nname=\"two\"\nlocal=\"./vendor/two\"\n");
    write_file(&env.manifest_path(), &m, false);

    env.skm().args(["sync", "--locked"]).assert().code(3);
}

#[test]
fn frozen_skill_missing_from_lock_errors() {
    // A skill present in the manifest but absent from the lock under --frozen
    // must error explicitly (not silently skip), pointing the user at `skm lock`.
    let env = Env::new();
    setup_local(&env);
    env.skm().arg("sync").assert().success();

    // Add a new skill to the manifest without relocking.
    let src = env.project.join("vendor/two");
    write_file(&src.join("SKILL.md"), "# two\n", false);
    let mut m = env.manifest_text();
    m.push_str("\n[[skills]]\nname=\"two\"\nlocal=\"./vendor/two\"\n");
    write_file(&env.manifest_path(), &m, false);

    env.skm()
        .args(["sync", "--frozen"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("is in manifest but not in lock"));
}
