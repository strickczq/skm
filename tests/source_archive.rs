//! Zip/tar archive sources: sha256 verification, the exec-bit policy ladder
//! (tar preserves, zip excludes from the hash), archive-vs-content drift under
//! --frozen, and top-level-dir stripping via `path`.

mod support;
use support::*;

#[test]
fn zip_sha256_verify_failure_aborts() {
    let env = Env::new();
    let zip = Archive::zip().file("SKILL.md", "# z\n", false).build();
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/my-tool.zip")
        .with_body(zip.clone())
        .create();
    let url = format!("{}/my-tool.zip", server.url());

    // Wrong declared sha256 → abort.
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::zip("z", &url).sha256(&"0".repeat(64)))
        .write_to(&env.manifest_path());
    env.skm()
        .arg("lock")
        .assert()
        .failure()
        .stderr(predicates::str::contains("sha256 mismatch"));

    // Correct sha256 → installs.
    let good = sha256_hex(&zip);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::zip("z", &url).sha256(&good))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    env.assert_installed("z");
    // Status no drift (zip exec policy constant).
    env.assert_clean();
}

#[test]
fn zip_exec_bit_not_in_hash() {
    // Two zips with the same content but different exec modes hash identically.
    let exec_zip = Archive::zip()
        .file("SKILL.md", "# z\n", false)
        .file("run.sh", "#!/bin/sh\n", true)
        .build();
    let plain_zip = Archive::zip()
        .file("SKILL.md", "# z\n", false)
        .file("run.sh", "#!/bin/sh\n", false)
        .build();

    let hash_of = |bytes: &[u8], name: &str| -> String {
        let env = Env::new();
        let mut server = mockito::Server::new();
        server.mock("GET", "/a.zip").with_body(bytes).create();
        let url = format!("{}/a.zip", server.url());
        Manifest::v1()
            .default_agents(&["agents"])
            .skill(Skill::zip(name, &url))
            .write_to(&env.manifest_path());
        env.skm().arg("lock").assert().success();
        env.lock_sha256(name)
    };

    assert_eq!(hash_of(&exec_zip, "z"), hash_of(&plain_zip, "z"));
}

#[test]
fn tar_preserves_exec_and_no_false_drift() {
    let env = Env::new();
    let tar = Archive::tar_gz()
        .file("SKILL.md", "# t\n", false)
        .file("run.sh", "#!/bin/sh\necho hi\n", true)
        .build();
    let mut server = mockito::Server::new();
    server.mock("GET", "/u.tar.gz").with_body(tar).create();
    let url = format!("{}/u.tar.gz", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::tar("t", &url))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(env.agents_root().join("t/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "tar exec bit preserved");

    env.assert_clean();
    env.skm().args(["sync", "--frozen"]).assert().success();
    env.assert_clean();
}

#[test]
fn archive_sha_drift_with_same_content_is_warn_only() {
    // Server repacks the archive (different bytes, same content). With
    // the cache wiped between syncs, default sync warns and re-uses; --frozen
    // accepts because content_sha256 still matches the lock.
    let env = Env::new();
    let tar_v1 = Archive::tar_gz_with(flate2::Compression::default())
        .file("SKILL.md", "# u\n", false)
        .file("note.txt", "hi\n", false)
        .build();
    let tar_v2 = Archive::tar_gz_with(flate2::Compression::none())
        .file("SKILL.md", "# u\n", false)
        .file("note.txt", "hi\n", false)
        .build();
    assert_ne!(tar_v1, tar_v2, "different gzip levels → different bytes");

    let mut server = mockito::Server::new();
    server.mock("GET", "/u.tar.gz").with_body(tar_v1).create();
    let url = format!("{}/u.tar.gz", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::tar("u", &url))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    let content_hash = env.lock_sha256("u");

    // Repack: same content, different bytes.
    server.reset();
    server
        .mock("GET", "/u.tar.gz")
        .with_body(tar_v2.clone())
        .create();
    // Wipe the cache so the next sync must re-download.
    env.skm().args(["cache", "clean"]).assert().success();

    // --frozen succeeds (archive sha drifted, content matches the lock).
    let out = env.skm().args(["sync", "--frozen"]).output().unwrap();
    assert!(
        out.status.success(),
        "frozen sync should succeed under archive_sha256 drift; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("archive_sha256") && stderr.contains("drifted"),
        "expected archive drift warning in stderr, got: {stderr}"
    );

    // Default sync rewrites the lock with the new archive_sha256; content_sha256
    // stays the same.
    server.reset();
    server.mock("GET", "/u.tar.gz").with_body(tar_v2).create();
    env.skm().args(["cache", "clean"]).assert().success();
    env.skm().arg("sync").assert().success();
    assert_eq!(
        env.lock_sha256("u"),
        content_hash,
        "content_sha256 should be stable across an archive repack"
    );
}

#[test]
fn archive_content_drift_under_frozen_aborts() {
    // Content actually changed → --frozen must abort (content gate).
    let env = Env::new();
    let tar_v1 = Archive::tar_gz()
        .file("SKILL.md", "# u v1\n", false)
        .build();
    let tar_v2 = Archive::tar_gz()
        .file("SKILL.md", "# u v2\n", false)
        .build();

    let mut server = mockito::Server::new();
    server.mock("GET", "/u.tar.gz").with_body(tar_v1).create();
    let url = format!("{}/u.tar.gz", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::tar("u", &url))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();

    // Swap content and wipe cache.
    server.reset();
    server.mock("GET", "/u.tar.gz").with_body(tar_v2).create();
    env.skm().args(["cache", "clean"]).assert().success();
    // Delete the on-disk skill so deploy is forced to materialize from cache.
    std::fs::remove_dir_all(env.agents_root().join("u")).unwrap();

    let out = env.skm().args(["sync", "--frozen"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "expected exit 3 for content drift under --frozen; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn manifest_declared_sha_mismatch_still_aborts_on_refetch() {
    // Manifest sha256 stays a hard gate even after lock. If the user pins
    // a sha and the server later serves different bytes, the resolve gate
    // aborts (this case forces resolve via --upgrade).
    let env = Env::new();
    let zip_v1 = Archive::zip().file("SKILL.md", "# v1\n", false).build();
    let zip_v2 = Archive::zip().file("SKILL.md", "# v2\n", false).build();
    let sha_v1 = sha256_hex(&zip_v1);

    let mut server = mockito::Server::new();
    server.mock("GET", "/z.zip").with_body(zip_v1).create();
    let url = format!("{}/z.zip", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::zip("z", &url).sha256(&sha_v1))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();

    // Replace bytes; declared sha is still v1.
    server.reset();
    server.mock("GET", "/z.zip").with_body(zip_v2).create();
    env.skm().args(["cache", "clean"]).assert().success();
    env.skm()
        .args(["lock", "--upgrade"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("sha256 mismatch"));
}

#[test]
fn archive_top_level_dir_with_path() {
    let env = Env::new();
    // Archive has a single top-level dir; use --subdir equivalent via manifest.
    let tar = Archive::tar_gz()
        .file("pkg-1.0/SKILL.md", "# p\n", false)
        .file("pkg-1.0/a.txt", "x\n", false)
        .build();
    let mut server = mockito::Server::new();
    server.mock("GET", "/p.tar.gz").with_body(tar).create();
    let url = format!("{}/p.tar.gz", server.url());
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::tar("p", &url).subdir("pkg-1.0"))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    env.assert_installed("p");
    assert!(!env.agents_root().join("p/pkg-1.0").exists());
}
