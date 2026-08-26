#![allow(clippy::expect_used)]

use assert_cmd::Command;
use serde_json::Value;

#[test]
fn empty_scan_is_successful_json() {
    let missing = tempfile::tempdir()
        .expect("临时目录")
        .path()
        .join("not-a-repo");
    let output = Command::cargo_bin("wtgc")
        .expect("找到 wtgc")
        .args(["--json", "--offline", "--repo"])
        .arg(&missing)
        .arg("scan")
        .output()
        .expect("执行 wtgc");

    assert!(
        output.status.success(),
        "空结果是正常状态：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("stdout 是 JSON");
    assert_eq!(report["repos"], serde_json::json!([]));
}

#[test]
fn explicit_repo_can_be_strictly_scoped() {
    let repo = wtgc::testkit::TempRepo::new();
    repo.write("a.txt", "x");
    repo.commit("init");

    let output = Command::cargo_bin("wtgc")
        .expect("找到 wtgc")
        .args(["--json", "--offline", "--repo"])
        .arg(&repo.root)
        .arg("scan")
        .output()
        .expect("执行 wtgc");

    assert!(
        output.status.success(),
        "扫描失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("stdout 是 JSON");
    let repos = report["repos"].as_array().expect("repos 数组");
    assert_eq!(repos.len(), 1, "显式范围不应混入默认种子目录下的其它仓库");
    assert_eq!(repos[0]["root"], repo.root.to_string_lossy().as_ref());
}

#[test]
fn run_executes_a_command_without_scanning_or_modifying_the_repo() {
    let repo = wtgc::testkit::TempRepo::new();
    repo.write("a.txt", "x");
    repo.commit("init");
    let config_dir = tempfile::tempdir().expect("临时配置目录");
    let config = config_dir.path().join("shared-cache.json");

    let output = Command::cargo_bin("wtgc")
        .expect("找到 wtgc")
        .env("WTGC_SHARED_CACHE_CONFIG", &config)
        .arg("--repo")
        .arg(&repo.root)
        .args(["run", "--", "git", "--version"])
        .output()
        .expect("执行 wtgc run");

    assert!(
        output.status.success(),
        "包装命令失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("git version"));
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn run_rejects_more_than_one_repository() {
    let repo = wtgc::testkit::TempRepo::new();
    repo.write("a.txt", "x");
    repo.commit("init");

    let output = Command::cargo_bin("wtgc")
        .expect("找到 wtgc")
        .arg("--repo")
        .arg(&repo.root)
        .arg("--repo")
        .arg(&repo.root)
        .args(["run", "--", "git", "--version"])
        .output()
        .expect("执行 wtgc run");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("一次只能指定一个"));
}
