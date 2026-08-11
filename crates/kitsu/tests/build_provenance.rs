//! Build-script regression tests use a dependency-free fixture and a private
//! target directory, so recursive Cargo invocations stay bounded and never
//! contend with this workspace's target lock.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

fn checked(command: &mut Command) -> Output {
    let debug = format!("{command:?}");
    let output = command.output().unwrap_or_else(|error| {
        panic!("cannot run {debug}: {error}");
    });
    assert!(
        output.status.success(),
        "{debug} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git<I, S>(cwd: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .arg("-c")
        .arg("user.name=Kitsu fixture")
        .arg("-c")
        .arg("user.email=fixture@example.invalid")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args);
    checked(&mut command)
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn create_fixture(repository: &Path) {
    let package = repository.join("crates/kitsu");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(
        repository.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/kitsu\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    std::fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"provenance-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .unwrap();
    std::fs::write(
        package.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"KITSU_GIT_SHA\")); }\n",
    )
    .unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"),
        package.join("build.rs"),
    )
    .unwrap();

    let mut lock = cargo();
    lock.current_dir(repository)
        .arg("generate-lockfile")
        .arg("--offline")
        .arg("--quiet");
    checked(&mut lock);

    git(repository, ["init", "--quiet"]);
    git(repository, ["add", "."]);
    git(repository, ["commit", "--quiet", "-m", "fixture A"]);
}

fn head(repository: &Path) -> String {
    String::from_utf8(git(repository, ["rev-parse", "--short=12", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn build_and_read(worktree: &Path, target: &Path) -> String {
    let mut build = cargo();
    build
        .current_dir(worktree)
        .arg("build")
        .arg("--offline")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(worktree.join("crates/kitsu/Cargo.toml"))
        .arg("--target-dir")
        .arg(target);
    checked(&mut build);

    let binary = target.join("debug").join(format!(
        "provenance-fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut run = Command::new(binary);
    String::from_utf8(checked(&mut run).stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn commit_unwatched_file(worktree: &Path) {
    std::fs::write(
        worktree.join("README.md"),
        "This commit intentionally does not touch Cargo build inputs.\n",
    )
    .unwrap();
    git(worktree, ["add", "README.md"]);
    git(worktree, ["commit", "--quiet", "-m", "fixture B"]);
}

#[test]
fn ordinary_checkout_rebuild_observes_a_new_commit_without_clean() {
    let fixture = tempfile::tempdir().unwrap();
    let repository = fixture.path().join("repository");
    let target = fixture.path().join("target");
    create_fixture(&repository);

    let a = head(&repository);
    assert_eq!(build_and_read(&repository, &target), a);

    commit_unwatched_file(&repository);
    let b = head(&repository);
    assert_ne!(a, b);
    assert_eq!(build_and_read(&repository, &target), b);

    let source = repository.join("crates/kitsu/src/main.rs");
    let clean_source = std::fs::read(&source).unwrap();
    let mut dirty_source = clean_source.clone();
    dirty_source.extend_from_slice(b"// relevant uncommitted source change\n");
    std::fs::write(&source, dirty_source).unwrap();
    assert_eq!(build_and_read(&repository, &target), format!("{b}-dirty"));
    std::fs::write(&source, clean_source).unwrap();
    assert_eq!(build_and_read(&repository, &target), b);
}

#[test]
fn linked_worktree_rebuild_observes_a_new_commit_without_clean() {
    let fixture = tempfile::tempdir().unwrap();
    let repository = fixture.path().join("repository");
    let worktree = fixture.path().join("worktree");
    let target = fixture.path().join("target");
    create_fixture(&repository);
    git(
        &repository,
        [
            "worktree",
            "add",
            "--quiet",
            "-b",
            "fixture-worktree",
            worktree.to_str().unwrap(),
        ],
    );

    let a = head(&worktree);
    assert_eq!(build_and_read(&worktree, &target), a);

    commit_unwatched_file(&worktree);
    let b = head(&worktree);
    assert_ne!(a, b);
    assert_eq!(build_and_read(&worktree, &target), b);
}
