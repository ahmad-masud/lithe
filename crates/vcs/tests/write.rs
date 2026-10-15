//! Staging, committing and blame, against a real repository.
//!
//! These go through `git` itself, so the only honest test is to make a
//! repository, write to it, and ask `git` whether the result is what was
//! intended.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use lithe_vcs::Repo;

struct Fixture(PathBuf);

impl Fixture {
    /// A repository with one committed file.
    fn new() -> Option<Self> {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping: git is not installed");
            return None;
        }
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lithe-vcs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let fixture = Self(path);

        fixture.git(&["init", "-q", "."]);
        fixture.git(&["config", "user.email", "test@example.com"]);
        fixture.git(&["config", "user.name", "Test Person"]);
        // Keep the test independent of the user's commit.gpgsign setting.
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.write("src/main.rs", "fn main() {\n    one();\n    two();\n}\n");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "initial"]);
        Some(fixture)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .output()
            .expect("git should run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn committed_contents_come_from_head_not_the_working_copy() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    std::fs::write(&file, "fn main() {\n    CHANGED();\n    two();\n}\n").unwrap();

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let committed = repo.committed(&file).expect("the file is tracked");
    assert!(
        committed.contains("one();") && !committed.contains("CHANGED"),
        "committed() returned the working copy, not HEAD"
    );

    let hunks = lithe_vcs::hunks(&committed, &std::fs::read_to_string(&file).unwrap());
    assert_eq!(hunks.len(), 1, "one line changed, got {hunks:?}");
    assert_eq!(hunks[0].line, 1);
}

#[test]
fn staging_then_committing_lands_in_history() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    std::fs::write(&file, "fn main() {\n    three();\n}\n").unwrap();

    let repo = Repo::discover(fixture.path()).expect("a repository");
    assert!(repo.dirty_count() > 0, "the edit should show as dirty");

    repo.stage(&file).expect("staging should succeed");
    let staged = fixture.git(&["diff", "--cached", "--name-only"]);
    assert!(staged.contains("src/main.rs"), "nothing was staged");

    let hash = repo
        .commit("change the call")
        .expect("commit should succeed");
    assert!(!hash.is_empty());

    // Ask git, not ourselves, whether it worked.
    let log = fixture.git(&["log", "--oneline", "-1"]);
    assert!(log.contains("change the call"), "git log says {log:?}");
    assert_eq!(repo.dirty_count(), 0, "the tree should be clean again");

    // And HEAD now matches what we wrote.
    let committed = repo.committed(&file).expect("still tracked");
    assert!(committed.contains("three();"));
}

#[test]
fn unstaging_leaves_the_working_copy_alone() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    std::fs::write(&file, "fn main() {\n    edited();\n}\n").unwrap();

    let repo = Repo::discover(fixture.path()).expect("a repository");
    repo.stage(&file).unwrap();
    repo.unstage(&file).expect("unstaging should succeed");

    assert!(
        fixture
            .git(&["diff", "--cached", "--name-only"])
            .trim()
            .is_empty(),
        "the file is still staged"
    );
    assert!(
        std::fs::read_to_string(&file).unwrap().contains("edited()"),
        "unstaging must not touch the working copy"
    );
}

#[test]
fn an_empty_commit_message_is_refused_before_git_sees_it() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("a repository");
    let error = repo.commit("   ").expect_err("should refuse");
    assert!(error.to_string().contains("message"), "got {error}");
}

#[test]
fn a_failing_commit_reports_gits_own_words() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("a repository");
    // Nothing staged, so git refuses.
    let error = repo.commit("nothing to do").expect_err("should fail");
    assert!(!error.to_string().is_empty(), "the reason was swallowed");
}

#[test]
fn blame_names_the_author_of_a_committed_line() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    let repo = Repo::discover(fixture.path()).expect("a repository");

    let blame = repo.blame_file(&file).expect("no blame");
    assert_eq!(blame.len(), 4, "one entry per line of the file");
    // All four lines came from the one commit, and it is stored once.
    assert_eq!(blame.commits.len(), 1, "the commit should be shared");

    let line = blame.line(1).expect("line 2 is committed").label();
    assert!(line.contains("Test Person"), "got {line:?}");
    assert!(
        line.contains("initial"),
        "the summary should be there: {line:?}"
    );
}

#[test]
fn blame_says_so_for_an_uncommitted_line() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    std::fs::write(
        &file,
        "fn main() {\n    one();\n    brand_new();\n    two();\n}\n",
    )
    .unwrap();

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let blame = repo.blame_file(&file).expect("no blame");

    let added = blame.line(2).expect("line 3 exists");
    assert!(added.uncommitted, "the new line is not in any commit");
    assert!(added.label().contains("not committed"), "got {added:?}");

    let old = blame.line(1).expect("line 2 exists");
    assert!(
        !old.uncommitted && old.author.contains("Test Person"),
        "the untouched lines keep their real blame: {old:?}"
    );
}

/// git's paths are slash-separated on every platform — its own status
/// output, a pathspec and a permalink all are — so ours have to be too.
#[test]
fn a_repository_path_is_spelled_the_way_git_spells_it() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("a repository");
    let nested = fixture.path().join("src").join("main.rs");
    assert_eq!(repo.relative(&nested).expect("inside"), "src/main.rs");
}

#[test]
fn a_path_outside_the_repository_is_rejected() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("a repository");
    let error = repo
        .stage(Path::new("/etc/hosts"))
        .expect_err("should refuse a path outside the worktree");
    assert!(error.to_string().contains("outside"), "got {error}");
}

#[test]
fn changes_are_split_into_staged_and_not() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.write("src/main.rs", "fn main() {\n    edited();\n}\n");
    fixture.write("untracked.txt", "hello\n");
    fixture.write("staged.rs", "fn s() {}\n");
    fixture.git(&["add", "staged.rs"]);

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let changes = repo.changes();

    let staged: Vec<_> = changes.iter().filter(|c| c.staged).collect();
    let unstaged: Vec<_> = changes.iter().filter(|c| !c.staged).collect();

    assert!(
        staged
            .iter()
            .any(|c| c.display == "staged.rs" && c.status == 'A'),
        "staged side: {staged:?}"
    );
    assert!(
        unstaged
            .iter()
            .any(|c| c.display == "src/main.rs" && c.status == 'M'),
        "unstaged side: {unstaged:?}"
    );
    assert!(
        unstaged
            .iter()
            .any(|c| c.display == "untracked.txt" && c.status == '?'),
        "untracked should be listed: {unstaged:?}"
    );
    // And every path is absolute, so opening one needs no extra work.
    assert!(changes.iter().all(|c| c.path.is_absolute()));
}

#[test]
fn a_file_both_staged_and_edited_shows_on_both_sides() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    std::fs::write(&file, "fn main() {\n    first();\n}\n").unwrap();
    fixture.git(&["add", "src/main.rs"]);
    std::fs::write(&file, "fn main() {\n    second();\n}\n").unwrap();

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let changes = repo.changes();
    let mine: Vec<_> = changes
        .iter()
        .filter(|c| c.display == "src/main.rs")
        .collect();
    assert_eq!(mine.len(), 2, "git reports both halves: {mine:?}");
    assert!(mine.iter().any(|c| c.staged));
    assert!(mine.iter().any(|c| !c.staged));
}

#[test]
fn a_clean_tree_has_no_changes() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("a repository");
    assert!(repo.changes().is_empty());
}

#[test]
fn a_path_with_a_space_survives_the_nul_separated_parse() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.write("a file with spaces.txt", "x\n");
    let repo = Repo::discover(fixture.path()).expect("a repository");
    let changes = repo.changes();
    assert!(
        changes
            .iter()
            .any(|c| c.display == "a file with spaces.txt"),
        "got {changes:?}"
    );
}

#[test]
fn the_log_reads_back_the_commits_that_were_made() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.write("second.rs", "fn s() {}\n");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "add second"]);

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let log = repo.log(10);

    assert_eq!(log.len(), 2, "two commits were made, got {log:?}");
    assert_eq!(log[0].summary, "add second", "newest first");
    assert_eq!(log[1].summary, "initial");
    assert_eq!(log[0].author, "Test Person");
    assert!(!log[0].short.is_empty() && log[0].hash.starts_with(&log[0].short));
    assert!(log[0].age.contains("ago") || log[0].age == "just now");
}

#[test]
fn a_summary_containing_separators_survives_parsing() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.write("x.rs", "\n");
    fixture.git(&["add", "."]);
    // Tabs and pipes in a message must not split the record.
    fixture.git(&["commit", "-qm", "fix: a\tb | c - d"]);

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let log = repo.log(1);
    assert_eq!(
        log[0].summary, "fix: a\tb | c - d",
        "got {:?}",
        log[0].summary
    );
}

#[test]
fn a_commits_diff_comes_back_as_lines() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let file = fixture.path().join("src/main.rs");
    std::fs::write(&file, "fn main() {\n    changed();\n    two();\n}\n").unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "change it"]);

    let repo = Repo::discover(fixture.path()).expect("a repository");
    let head = repo.log(1)[0].hash.clone();
    let diff = repo.commit_diff(&head);

    assert!(
        diff.iter()
            .any(|l| l.kind == lithe_vcs::LineKind::Removed && l.text.contains("one()")),
        "the removed line is missing: {diff:?}"
    );
    assert!(
        diff.iter()
            .any(|l| l.kind == lithe_vcs::LineKind::Added && l.text.contains("changed()")),
        "the added line is missing"
    );
    // Context lines carry numbers for both sides.
    let context = diff
        .iter()
        .find(|l| l.kind == lithe_vcs::LineKind::Context)
        .expect("some context");
    assert!(context.old.is_some() && context.new.is_some());
}

#[test]
fn an_unknown_revision_yields_nothing_rather_than_panicking() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("a repository");
    assert!(repo.commit_diff("deadbeefdeadbeef").is_empty());
}

/// `View File History` is only useful if it is this file's history and
/// not the project's, so that is what is checked.
#[test]
fn a_files_log_holds_only_the_commits_that_touched_it() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("no repository");

    fixture.write("src/main.rs", "fn main() {\n    one();\n}\n");
    fixture.git(&["commit", "-qam", "drop two"]);
    fixture.write("README.md", "# readme\n");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "add a readme"]);

    let whole = repo.log(10);
    assert_eq!(whole.len(), 3, "got {:?}", summaries(&whole));

    let one = repo.file_log(&fixture.path().join("src/main.rs"), 10);
    let said = summaries(&one);
    assert_eq!(said, vec!["drop two", "initial"], "got {said:?}");
    assert!(
        !said.contains(&"add a readme".to_string()),
        "a commit that did not touch the file is in its history"
    );
}

#[test]
fn a_renamed_file_keeps_the_history_it_had_before() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("no repository");

    fixture.git(&["mv", "src/main.rs", "src/app.rs"]);
    fixture.git(&["commit", "-qm", "rename"]);

    let said = summaries(&repo.file_log(&fixture.path().join("src/app.rs"), 10));
    assert!(
        said.contains(&"initial".to_string()),
        "--follow should reach back past the rename: {said:?}"
    );
}

#[test]
fn head_is_the_commit_a_permalink_would_point_at() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let repo = Repo::discover(fixture.path()).expect("no repository");

    let head = repo.head().expect("no HEAD");
    assert_eq!(head.len(), 40, "not a full hash: {head}");
    assert_eq!(
        Some(head.as_str()),
        repo.log(1).first().map(|c| c.hash.as_str())
    );
}

fn summaries(commits: &[lithe_vcs::Commit]) -> Vec<String> {
    commits.iter().map(|c| c.summary.clone()).collect()
}

#[test]
fn init_makes_a_repository_where_there_was_none() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let path = std::env::temp_dir().join(format!("lithe-vcs-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("a.rs"), "fn main() {}\n").unwrap();

    assert!(
        Repo::discover(&path).is_none(),
        "the fixture should start outside a repository"
    );

    let repo = Repo::init(&path).expect("git init");
    assert_eq!(
        repo.workdir().canonicalize().ok(),
        path.canonicalize().ok(),
        "the repository should be rooted where it was asked for"
    );
    // And it is a repository the rest of this crate can use: the new
    // file shows up as an untracked change.
    assert!(
        repo.changes().iter().any(|c| c.display.contains("a.rs")),
        "got {:?}",
        repo.changes()
    );
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn discarding_puts_a_file_back_to_its_committed_state() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let path = fixture.path().join("src/main.rs");
    let committed = std::fs::read_to_string(&path).unwrap();
    fixture.write("src/main.rs", "fn main() {\n    broken();\n}\n");

    let repo = Repo::discover(fixture.path()).unwrap();
    assert!(!repo.changes().is_empty(), "the edit should show");

    repo.discard(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), committed);
    assert!(repo.changes().is_empty(), "and nothing is left to commit");
}

#[test]
fn discarding_takes_a_staged_change_with_it() {
    // `git restore` alone only touches the working copy, so a staged
    // change would come back the moment anything read the index.
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let path = fixture.path().join("src/main.rs");
    let committed = std::fs::read_to_string(&path).unwrap();
    fixture.write("src/main.rs", "fn main() {\n    staged();\n}\n");
    fixture.git(&["add", "src/main.rs"]);

    let repo = Repo::discover(fixture.path()).unwrap();
    assert!(
        repo.changes().iter().any(|c| c.staged),
        "staged, to begin with"
    );

    repo.discard(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), committed);
    assert!(repo.changes().is_empty(), "both halves gone");
}

#[test]
fn discarding_an_untracked_file_deletes_it() {
    // There is no committed version to go back to, so this is the only
    // thing discarding it can mean.
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.write("new.rs", "fn fresh() {}\n");
    let path = fixture.path().join("new.rs");

    let repo = Repo::discover(fixture.path()).unwrap();
    repo.discard(&path).unwrap();
    assert!(!path.exists(), "an untracked file is removed");
    assert!(repo.changes().is_empty());
}
