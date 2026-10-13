//! Version control, through gitoxide.
//!
//! Reads go through gitoxide: pure Rust, no libgit2, and nothing is read
//! until it is asked for. The repository is discovered when a project
//! opens, and a file's committed contents are fetched only when that file
//! is on screen. The gutter runs on every frame and must never stall on a
//! subprocess.
//!
//! Writes — staging, committing — shell out to `git`. That is a deliberate
//! split, not an inconsistency. gix 0.88 exposes `commit()` but no way to
//! turn an index into a tree, so committing through it would mean building
//! tree objects and moving refs by hand. A bug there costs someone their
//! work, whereas a bug in the gutter costs a redraw. Staging and committing
//! are one-off actions a person asks for, where thirty milliseconds of
//! subprocess is invisible, and `git` is the reference implementation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use imara_diff::{Algorithm, Diff, InternedInput};

/// Keeps `git` from opening a console window of its own.
///
/// `git` is a console program, and Windows gives one a console unless told
/// not to — so staging a file from a windowed application would flash a
/// black box over the editor. Its output is captured either way.
#[cfg(windows)]
fn no_console(command: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn no_console(command: &mut Command) -> &mut Command {
    command
}

/// How a run of lines differs from the committed version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    /// Lines were deleted here. There is nothing to paint over, so this
    /// marks the line the deletion sits above.
    Removed,
}

/// A run of changed lines, in the working copy's coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// 0-based first line.
    pub line: usize,
    /// How many lines the run covers. Zero for a deletion.
    pub lines: usize,
    pub change: Change,
}

pub struct Repo {
    inner: gix::Repository,
    workdir: PathBuf,
}

impl Repo {
    /// Finds the repository containing `path`, if there is one.
    pub fn discover(path: &Path) -> Option<Self> {
        let inner = gix::discover(path)
            .map_err(|e| log::debug!("no git repository at {}: {e}", path.display()))
            .ok()?;
        let workdir = inner.workdir()?.to_path_buf();
        Some(Self { inner, workdir })
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Starts a repository at `path`, and returns it.
    ///
    /// Shelled out like every other write here: `git init` sets up hooks,
    /// a description, the configured default branch name and whatever
    /// templates are configured, and reimplementing that against gix
    /// would mean a repository subtly unlike the one `git init` makes.
    pub fn init(path: &Path) -> Result<Self, GitError> {
        let mut command = std::process::Command::new("git");
        command.args(["init"]).current_dir(path);
        let output = no_console(&mut command)
            .output()
            .map_err(|e| GitError(format!("could not run git: {e}")))?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            return Err(GitError(message.trim().to_string()));
        }
        Self::discover(path)
            .ok_or_else(|| GitError("git init reported success but left no repository".to_string()))
    }

    /// The current branch, or a short commit id when HEAD is detached.
    pub fn branch(&self) -> Option<String> {
        if let Ok(Some(name)) = self.inner.head_name() {
            return Some(name.shorten().to_string());
        }
        let id = self.inner.head_id().ok()?;
        Some(format!("detached {}", id.to_hex_with_len(7)))
    }

    /// A file's contents as of HEAD. `None` when the file is untracked, is
    /// not valid UTF-8, or there is no commit yet.
    pub fn committed(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.workdir).ok()?;
        let tree = self.inner.head_tree().ok()?;
        let entry = tree.lookup_entry_by_path(relative).ok()??;
        let object = entry.object().ok()?;
        String::from_utf8(object.data.clone()).ok()
    }

    /// Whether the path is tracked at HEAD at all.
    pub fn is_tracked(&self, path: &Path) -> bool {
        self.committed(path).is_some()
    }
}

/// Line-level difference between the committed text and the buffer.
///
/// Returned in the working copy's line numbers, which is what the gutter
/// needs; a deletion is reported at the line it collapsed to.
pub fn hunks(committed: &str, current: &str) -> Vec<Hunk> {
    let input = InternedInput::new(committed, current);
    let diff = Diff::compute(Algorithm::Histogram, &input);

    diff.hunks()
        .map(|hunk| {
            let before = hunk.before.len();
            let after = hunk.after.len();
            let line = hunk.after.start as usize;
            if before == 0 {
                Hunk {
                    line,
                    lines: after,
                    change: Change::Added,
                }
            } else if after == 0 {
                Hunk {
                    line: line.saturating_sub(1),
                    lines: 0,
                    change: Change::Removed,
                }
            } else {
                Hunk {
                    line,
                    lines: after,
                    change: Change::Modified,
                }
            }
        })
        .collect()
}

/// A line in a unified diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// A gap where unchanged lines were elided.
    Skipped,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number in the committed version, where there is one.
    pub old: Option<usize>,
    /// Line number in the working copy, where there is one.
    pub new: Option<usize>,
    pub text: String,
}

/// A unified diff with `context` unchanged lines around each change.
///
/// Whole-file diffs are mostly unchanged lines, and scrolling past a
/// thousand of them to find the edit is not reading a diff. Runs further
/// apart than twice the context are separated by a [`LineKind::Skipped`]
/// marker rather than joined.
pub fn unified(before: &str, after: &str, context: usize) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = before.lines().collect();
    let new_lines: Vec<&str> = after.lines().collect();

    let input = InternedInput::new(before, after);
    let diff = Diff::compute(Algorithm::Histogram, &input);
    let hunks: Vec<_> = diff.hunks().collect();
    if hunks.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<DiffLine> = Vec::new();
    // Where we have copied up to in each file.
    let mut old_at = 0usize;
    let mut new_at = 0usize;

    for hunk in hunks {
        let change_new = hunk.after.start as usize;
        let lead = change_new.saturating_sub(context);

        // Elide the unchanged run before this hunk when it is long.
        if lead > new_at {
            out.push(DiffLine {
                kind: LineKind::Skipped,
                old: None,
                new: None,
                text: format!("… {} unchanged lines", lead - new_at),
            });
            old_at += lead - new_at;
            new_at = lead;
        }
        while new_at < change_new {
            out.push(DiffLine {
                kind: LineKind::Context,
                old: Some(old_at + 1),
                new: Some(new_at + 1),
                text: new_lines
                    .get(new_at)
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            });
            old_at += 1;
            new_at += 1;
        }

        for index in hunk.before.clone() {
            out.push(DiffLine {
                kind: LineKind::Removed,
                old: Some(index as usize + 1),
                new: None,
                text: old_lines
                    .get(index as usize)
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        for index in hunk.after.clone() {
            out.push(DiffLine {
                kind: LineKind::Added,
                old: None,
                new: Some(index as usize + 1),
                text: new_lines
                    .get(index as usize)
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        old_at = hunk.before.end as usize;
        new_at = hunk.after.end as usize;
    }

    // Trailing context, then whatever is left is elided.
    let tail = (new_at + context).min(new_lines.len());
    while new_at < tail {
        out.push(DiffLine {
            kind: LineKind::Context,
            old: Some(old_at + 1),
            new: Some(new_at + 1),
            text: new_lines
                .get(new_at)
                .copied()
                .unwrap_or_default()
                .to_string(),
        });
        old_at += 1;
        new_at += 1;
    }
    if new_at < new_lines.len() {
        out.push(DiffLine {
            kind: LineKind::Skipped,
            old: None,
            new: None,
            text: format!("… {} unchanged lines", new_lines.len() - new_at),
        });
    }
    out
}

/// Pairs a unified diff into two columns.
///
/// A run of removals followed by a run of additions is one edit shown
/// twice in a unified diff; side by side they belong on the same row. The
/// runs are rarely the same length, so the shorter side gets blanks.
///
/// Returns indices into `lines` rather than references, so callers can
/// keep borrowing the original list.
pub fn pair_columns(lines: &[DiffLine]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        match lines[i].kind {
            LineKind::Context | LineKind::Skipped => {
                out.push((Some(i), Some(i)));
                i += 1;
            }
            _ => {
                // Collect this change: everything removed, then everything
                // added, before the next context line.
                let removed_start = i;
                while i < lines.len() && lines[i].kind == LineKind::Removed {
                    i += 1;
                }
                let removed = removed_start..i;
                let added_start = i;
                while i < lines.len() && lines[i].kind == LineKind::Added {
                    i += 1;
                }
                let added = added_start..i;

                let rows = removed.len().max(added.len());
                for row in 0..rows {
                    out.push((removed.clone().nth(row), added.clone().nth(row)));
                }
                // A lone Added run with no Removed before it still
                // advances; guard against standing still.
                if rows == 0 {
                    i += 1;
                }
            }
        }
    }
    out
}

/// One path that differs from HEAD or the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    /// Relative to the working tree, for display.
    pub display: String,
    /// git's own letter: M, A, D, R, ? and so on.
    pub status: char,
    pub staged: bool,
}

/// One entry in the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub summary: String,
    pub author: String,
    /// Already rendered as "3 days ago".
    pub age: String,
}

/// Why a git write failed, in terms worth showing someone.
#[derive(Debug)]
pub struct GitError(pub String);

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GitError {}

impl Repo {
    /// Runs `git` in the working tree and returns its stdout.
    fn git(&self, args: &[&str]) -> Result<String, GitError> {
        let mut command = std::process::Command::new("git");
        command.args(args).current_dir(&self.workdir);
        let output = no_console(&mut command)
            .output()
            .map_err(|e| GitError(format!("could not run git: {e}")))?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        // git's own message is better than anything this layer could
        // invent, so it is passed through rather than summarised.
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        Err(GitError(if message.is_empty() {
            format!("git {} failed", args.first().copied().unwrap_or("?"))
        } else {
            message.lines().next().unwrap_or(message).to_string()
        }))
    }

    /// Stages one file.
    pub fn stage(&self, path: &Path) -> Result<(), GitError> {
        let relative = self.relative(path)?;
        self.git(&["add", "--", &relative]).map(|_| ())
    }

    /// Unstages one file, leaving the working copy alone.
    pub fn unstage(&self, path: &Path) -> Result<(), GitError> {
        let relative = self.relative(path)?;
        self.git(&["restore", "--staged", "--", &relative])
            .map(|_| ())
    }

    /// Commits what is staged. Returns the short hash.
    pub fn commit(&self, message: &str) -> Result<String, GitError> {
        if message.trim().is_empty() {
            return Err(GitError("a commit needs a message".into()));
        }
        self.git(&["commit", "-m", message])?;
        Ok(self
            .git(&["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string())
    }

    /// The most recent `limit` commits on the current branch.
    pub fn log(&self, limit: usize) -> Vec<Commit> {
        self.log_args(limit, &[])
    }

    /// The commits that touched one path, newest first.
    ///
    /// `--follow` so a rename does not cut the history short, which is
    /// the case where asking for a file's history matters most.
    pub fn file_log(&self, path: &Path, limit: usize) -> Vec<Commit> {
        let relative = path.strip_prefix(self.workdir()).unwrap_or(path);
        let Some(relative) = relative.to_str() else {
            return Vec::new();
        };
        self.log_args(limit, &["--follow", "--", relative])
    }

    fn log_args(&self, limit: usize, extra: &[&str]) -> Vec<Commit> {
        // A record separator git will not emit itself, so a summary
        // containing a tab or a newline cannot split the record.
        let format = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e";
        let count = format!("-n{limit}");
        let mut args = vec!["log", count.as_str(), format];
        args.extend_from_slice(extra);
        let Ok(output) = self.git(&args) else {
            return Vec::new();
        };
        output
            .split('\u{1e}')
            .map(str::trim)
            .filter(|record| !record.is_empty())
            .filter_map(|record| {
                let mut parts = record.split('\u{1f}');
                let hash = parts.next()?.to_string();
                let short = parts.next()?.to_string();
                let author = parts.next()?.to_string();
                let age = parts
                    .next()
                    .and_then(|t| t.parse::<i64>().ok())
                    .map(relative_age)
                    .unwrap_or_default();
                let summary = parts.next().unwrap_or_default().to_string();
                Some(Commit {
                    hash,
                    short,
                    author,
                    age,
                    summary,
                })
            })
            .collect()
    }

    /// What one commit changed, as diff lines.
    ///
    /// Parsed from git's own unified output rather than re-diffing the
    /// trees here: for a merge or a rename, git's answer is the correct
    /// one and ours would not be.
    pub fn commit_diff(&self, hash: &str) -> Vec<DiffLine> {
        let Ok(output) = self.git(&["show", "--format=", "--unified=3", hash]) else {
            return Vec::new();
        };
        parse_unified(&output)
    }

    /// Everything `git status` would list, split into staged and not.
    ///
    /// A path edited *and* staged appears twice, once on each side, which
    /// is what git means and what lets you stage some of a file's history
    /// and keep working on the rest.
    pub fn changes(&self) -> Vec<FileChange> {
        let Ok(output) = self.git(&["status", "--porcelain=v1", "-z"]) else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        // -z gives NUL-separated records, so paths with spaces or quotes
        // need no unescaping.
        let mut records = output.split('\0').filter(|r| !r.is_empty());

        while let Some(record) = records.next() {
            if record.len() < 3 {
                continue;
            }
            let bytes: Vec<char> = record.chars().collect();
            let (index, worktree) = (bytes[0], bytes[1]);
            let mut name: String = record[3..].to_string();

            // A rename record is followed by its original path, which is
            // consumed here so it is not read as a change of its own.
            if index == 'R' || worktree == 'R' {
                records.next();
            }
            if name.is_empty() {
                continue;
            }
            // Directories come back with a trailing slash for untracked
            // trees; show them as they are.
            if name.ends_with('/') {
                name.pop();
            }

            let path = self.workdir.join(&name);
            if index == '?' && worktree == '?' {
                changes.push(FileChange {
                    path,
                    display: name,
                    status: '?',
                    staged: false,
                });
                continue;
            }
            if index != ' ' {
                changes.push(FileChange {
                    path: path.clone(),
                    display: name.clone(),
                    status: index,
                    staged: true,
                });
            }
            if worktree != ' ' {
                changes.push(FileChange {
                    path,
                    display: name,
                    status: worktree,
                    staged: false,
                });
            }
        }
        changes
    }

    /// How many paths differ from the index plus how many are staged, for
    /// the status bar.
    pub fn dirty_count(&self) -> usize {
        self.git(&["status", "--porcelain"])
            .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    /// Who last touched a line, 1-based. `None` for an uncommitted line.
    /// The commit HEAD points at, for a permalink that keeps working.
    pub fn head(&self) -> Option<String> {
        let output = self.git(&["rev-parse", "HEAD"]).ok()?;
        let hash = output.trim();
        (!hash.is_empty()).then(|| hash.to_string())
    }

    /// A browsable URL for a line, built from the origin remote.
    ///
    /// Only the hosts whose URL shape we actually know: guessing at an
    /// unknown host produces a link that looks right and 404s, which is
    /// worse than saying we cannot.
    pub fn permalink(&self, path: &Path, line: usize) -> Option<String> {
        let remote = self.git(&["remote", "get-url", "origin"]).ok()?;
        let hash = self.head()?;
        let relative = self.relative(path).ok()?;
        remote_link(remote.trim(), &hash, &relative, line)
    }

    /// Who last touched each line of a file.
    ///
    /// The whole file in one `git blame`, not a subprocess per line: the
    /// annotation follows the cursor, and spawning git every time you
    /// press Down is not a thing this editor is allowed to do.
    pub fn blame_file(&self, path: &Path) -> Option<FileBlame> {
        let relative = self.relative(path).ok()?;
        let out = self.git(&["blame", "--porcelain", "--", &relative]).ok()?;
        Some(parse_blame(&out))
    }

    /// A path as git spells it: relative to the working tree, with forward
    /// slashes.
    ///
    /// The separator matters beyond looks. git's own paths — `git status`,
    /// a pathspec, a permalink on github.com — use `/` on every platform,
    /// so a Windows path handed back as `crates\app\src\main.rs` would
    /// disagree with the panel listing it, and `\` is an escape character
    /// in a pathspec rather than a separator.
    pub fn relative(&self, path: &Path) -> Result<String, GitError> {
        path.strip_prefix(&self.workdir)
            .map(|p| {
                let text = p.to_string_lossy();
                if std::path::MAIN_SEPARATOR == '/' {
                    text.into_owned()
                } else {
                    text.replace(std::path::MAIN_SEPARATOR, "/")
                }
            })
            .map_err(|_| GitError(format!("{} is outside the repository", path.display())))
    }
}

/// Turns git's unified diff text into the same lines `unified` produces.
fn parse_unified(text: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let (mut old_no, mut new_no) = (0usize, 0usize);

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            // "@@ -a,b +c,d @@" — the two starting line numbers.
            let numbers: Vec<&str> = rest.split(['-', '+', ',', ' ', '@']).collect();
            let parsed: Vec<usize> = numbers
                .iter()
                .filter(|n| !n.is_empty())
                .filter_map(|n| n.parse().ok())
                .collect();
            old_no = parsed.first().copied().unwrap_or(1);
            new_no = parsed.get(2).copied().unwrap_or(1);
            out.push(DiffLine {
                kind: LineKind::Skipped,
                old: None,
                new: None,
                text: line.to_string(),
            });
            continue;
        }
        // File headers, which are structure rather than content.
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("similarity ")
            || line.starts_with("rename ")
        {
            out.push(DiffLine {
                kind: LineKind::Skipped,
                old: None,
                new: None,
                text: line.to_string(),
            });
            continue;
        }

        let (kind, body) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            _ => continue,
        };
        let (old, new) = match kind {
            LineKind::Added => {
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                old_no += 1;
                (Some(old_no - 1), None)
            }
            _ => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
        };
        out.push(DiffLine {
            kind,
            old,
            new,
            text: body.to_string(),
        });
    }
    out
}

/// "3 days ago" and the like, from a unix timestamp.
fn relative_age(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(timestamp);
    let seconds = (now - timestamp).max(0);
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let (count, unit) = match seconds {
        s if s < MINUTE => return "just now".into(),
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < MONTH => (s / DAY, "day"),
        s if s < YEAR => (s / MONTH, "month"),
        s => (s / YEAR, "year"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

/// One commit, as a blame annotation needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCommit {
    pub author: String,
    /// Already phrased, e.g. "3 days ago".
    pub when: String,
    pub summary: String,
    /// True for the line you have edited but not committed.
    pub uncommitted: bool,
}

impl BlameCommit {
    /// One line, for showing beside the code.
    pub fn label(&self) -> String {
        if self.uncommitted {
            return "not committed yet".into();
        }
        if self.summary.is_empty() {
            format!("{}, {}", self.author, self.when)
        } else {
            format!("{}, {} · {}", self.author, self.when, self.summary)
        }
    }
}

/// A whole file's blame.
///
/// Commits are stored once and referenced by index, because a file is
/// usually a handful of commits over a great many lines — the table for a
/// ten-thousand-line file is forty kilobytes, not ten thousand strings.
#[derive(Debug, Default)]
pub struct FileBlame {
    pub commits: Vec<BlameCommit>,
    /// One index into `commits` per line, in file order.
    lines: Vec<u32>,
}

impl FileBlame {
    /// The commit for a 0-based line.
    pub fn line(&self, line: usize) -> Option<&BlameCommit> {
        self.commits.get(*self.lines.get(line)? as usize)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Reads `git blame --porcelain`.
///
/// Each line of the file begins with `<sha> <orig> <final> [count]`, and
/// a commit's details follow only the first time its sha appears — which
/// is exactly the deduplication we want, so it is kept rather than
/// flattened back out.
fn parse_blame(text: &str) -> FileBlame {
    let mut blame = FileBlame::default();
    let mut index: HashMap<String, u32> = HashMap::new();
    let mut current: Option<u32> = None;

    for entry in text.lines() {
        if entry.starts_with('\t') {
            // The content line ends a record; the header above it said
            // which line of the file this is.
            continue;
        }
        if let Some(rest) = entry.strip_prefix("author ") {
            if let Some(at) = current {
                blame.commits[at as usize].author = rest.to_string();
            }
            continue;
        }
        if let Some(rest) = entry.strip_prefix("summary ") {
            if let Some(at) = current {
                blame.commits[at as usize].summary = rest.to_string();
            }
            continue;
        }
        if let Some(rest) = entry.strip_prefix("author-time ") {
            if let (Some(at), Ok(time)) = (current, rest.parse::<i64>()) {
                blame.commits[at as usize].when = relative_age(time);
            }
            continue;
        }

        // A header: 40 hex characters, then the line numbers.
        let mut parts = entry.split(' ');
        let Some(sha) = parts.next() else { continue };
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        // The final line number, 1-based, is the third field.
        let Some(final_line) = parts.nth(1).and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };

        let at = *index.entry(sha.to_string()).or_insert_with(|| {
            blame.commits.push(BlameCommit {
                author: String::new(),
                when: String::new(),
                summary: String::new(),
                // git uses an all-zero sha for a line that is not in any
                // commit yet.
                uncommitted: sha.bytes().all(|b| b == b'0'),
            });
            (blame.commits.len() - 1) as u32
        });
        current = Some(at);

        if blame.lines.len() < final_line {
            blame.lines.resize(final_line, at);
        }
        blame.lines[final_line - 1] = at;
    }
    blame
}

/// A browsable URL for one line, from a remote URL and a commit.
///
/// Only the hosts whose URL shape we actually know. Guessing at an
/// unknown host produces a link that looks right and 404s, which is
/// worse than saying we cannot — so an unknown host returns nothing.
pub fn remote_link(remote: &str, hash: &str, relative: &str, line: usize) -> Option<String> {
    // git@host:owner/repo.git and https://host/owner/repo.git both reduce
    // to a host followed by a path.
    let rest = remote
        .strip_prefix("git@")
        .map(|r| r.replacen(':', "/", 1))
        .or_else(|| remote.strip_prefix("ssh://git@").map(str::to_string))
        .or_else(|| remote.strip_prefix("https://").map(str::to_string))
        .or_else(|| remote.strip_prefix("http://").map(str::to_string))?;
    let rest = rest.strip_suffix('/').unwrap_or(&rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (host, repo) = rest.split_once('/')?;
    if repo.is_empty() {
        return None;
    }

    match host {
        "github.com" => Some(format!(
            "https://github.com/{repo}/blob/{hash}/{relative}#L{line}"
        )),
        "gitlab.com" => Some(format!(
            "https://gitlab.com/{repo}/-/blob/{hash}/{relative}#L{line}"
        )),
        "bitbucket.org" => Some(format!(
            "https://bitbucket.org/{repo}/src/{hash}/{relative}#lines-{line}"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_file_has_no_hunks() {
        let text = "one\ntwo\nthree\n";
        assert!(hunks(text, text).is_empty());
    }

    #[test]
    fn added_lines_are_reported_where_they_land() {
        let before = "one\ntwo\n";
        let after = "one\ninserted\ntwo\n";
        let found = hunks(before, after);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].change, Change::Added);
        assert_eq!(found[0].line, 1, "the new line is the second one");
        assert_eq!(found[0].lines, 1);
    }

    #[test]
    fn modified_lines_are_modified_not_add_plus_remove() {
        let found = hunks("one\ntwo\n", "one\nTWO\n");
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].change, Change::Modified);
        assert_eq!(found[0].line, 1);
    }

    #[test]
    fn deletions_mark_the_line_above() {
        let found = hunks("one\ntwo\nthree\n", "one\nthree\n");
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].change, Change::Removed);
        assert_eq!(found[0].lines, 0, "a deletion covers no visible lines");
    }

    #[test]
    fn several_edits_produce_several_hunks() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let after = "a\nB\nc\nd\ne\nf\nG\nh\n";
        let found = hunks(before, after);
        assert_eq!(found.len(), 2, "distant edits should not merge: {found:?}");
    }

    #[test]
    fn a_new_file_is_entirely_added() {
        let found = hunks("", "one\ntwo\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].change, Change::Added);
    }

    #[test]
    fn this_repository_is_discovered_if_there_is_one() {
        // The crate lives inside a checkout when developed, but not when
        // vendored, so the absence of a repository is not a failure.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Some(repo) = Repo::discover(here) {
            assert!(repo.workdir().is_dir());
            // A branch name, or a detached-head description; either way
            // something readable.
            assert!(repo.branch().is_some_and(|b| !b.is_empty()));
        } else {
            eprintln!("skipping: not inside a git repository");
        }
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    fn kinds(lines: &[DiffLine]) -> Vec<LineKind> {
        lines.iter().map(|l| l.kind).collect()
    }

    #[test]
    fn an_unchanged_file_diffs_to_nothing() {
        assert!(unified("a\nb\n", "a\nb\n", 3).is_empty());
    }

    #[test]
    fn a_change_is_shown_as_a_removal_then_an_addition() {
        let out = unified("one\ntwo\nthree\n", "one\nTWO\nthree\n", 1);
        assert_eq!(
            kinds(&out),
            vec![
                LineKind::Context,
                LineKind::Removed,
                LineKind::Added,
                LineKind::Context
            ],
            "got {:?}",
            out.iter().map(|l| (&l.kind, &l.text)).collect::<Vec<_>>()
        );
        assert_eq!(out[1].text, "two");
        assert_eq!(out[2].text, "TWO");
    }

    #[test]
    fn both_sides_carry_their_own_line_numbers() {
        let out = unified("a\nb\nc\n", "a\nX\nc\n", 1);
        let removed = out.iter().find(|l| l.kind == LineKind::Removed).unwrap();
        let added = out.iter().find(|l| l.kind == LineKind::Added).unwrap();
        assert_eq!((removed.old, removed.new), (Some(2), None));
        assert_eq!((added.old, added.new), (None, Some(2)));
    }

    #[test]
    fn long_unchanged_runs_are_elided_rather_than_shown() {
        let before: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut after: Vec<String> = before.lines().map(|l| format!("{l}\n")).collect();
        after[100] = "CHANGED\n".to_string();
        let after: String = after.concat();

        let out = unified(&before, &after, 2);
        assert!(
            out.len() < 20,
            "a one-line change should not print the file: {} lines",
            out.len()
        );
        assert!(
            out.iter().any(|l| l.kind == LineKind::Skipped),
            "the unchanged bulk should be elided"
        );
        assert!(out.iter().any(|l| l.text == "CHANGED"));
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let out = unified("", "one\ntwo\n", 3);
        assert!(
            out.iter().all(|l| l.kind == LineKind::Added),
            "{:?}",
            kinds(&out)
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn two_distant_changes_are_separated_by_a_gap() {
        let before: String = (0..60).map(|i| format!("l{i}\n")).collect();
        let mut after: Vec<String> = before.lines().map(|l| format!("{l}\n")).collect();
        after[5] = "FIRST\n".into();
        after[50] = "SECOND\n".into();
        let after: String = after.concat();

        let out = unified(&before, &after, 2);
        let skips = out.iter().filter(|l| l.kind == LineKind::Skipped).count();
        assert!(skips >= 2, "expected gaps around and between, got {skips}");
    }
}

#[cfg(test)]
mod pair_tests {
    use super::*;

    fn lines(spec: &[(LineKind, &str)]) -> Vec<DiffLine> {
        spec.iter()
            .map(|(kind, text)| DiffLine {
                kind: *kind,
                old: None,
                new: None,
                text: (*text).to_string(),
            })
            .collect()
    }

    #[test]
    fn context_appears_on_both_sides() {
        let l = lines(&[(LineKind::Context, "same")]);
        assert_eq!(pair_columns(&l), vec![(Some(0), Some(0))]);
    }

    #[test]
    fn a_replacement_lines_up_on_one_row() {
        let l = lines(&[(LineKind::Removed, "old"), (LineKind::Added, "new")]);
        assert_eq!(pair_columns(&l), vec![(Some(0), Some(1))]);
    }

    #[test]
    fn uneven_runs_leave_blanks_on_the_shorter_side() {
        let l = lines(&[
            (LineKind::Removed, "a"),
            (LineKind::Added, "x"),
            (LineKind::Added, "y"),
            (LineKind::Added, "z"),
        ]);
        assert_eq!(
            pair_columns(&l),
            vec![(Some(0), Some(1)), (None, Some(2)), (None, Some(3))]
        );
    }

    #[test]
    fn a_pure_deletion_has_an_empty_right_column() {
        let l = lines(&[(LineKind::Removed, "gone"), (LineKind::Removed, "too")]);
        assert_eq!(pair_columns(&l), vec![(Some(0), None), (Some(1), None)]);
    }

    #[test]
    fn every_line_is_accounted_for_exactly_once() {
        let l = lines(&[
            (LineKind::Context, "1"),
            (LineKind::Removed, "2"),
            (LineKind::Removed, "3"),
            (LineKind::Added, "4"),
            (LineKind::Context, "5"),
            (LineKind::Added, "6"),
            (LineKind::Skipped, "..."),
        ]);
        let pairs = pair_columns(&l);
        let mut seen: Vec<usize> = pairs.iter().flat_map(|(a, b)| [*a, *b]).flatten().collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            l.len(),
            "some line was dropped or duplicated: {pairs:?}"
        );
    }

    #[test]
    fn an_empty_diff_pairs_to_nothing() {
        assert!(pair_columns(&[]).is_empty());
    }
}

#[cfg(test)]
mod permalink_tests {
    use super::remote_link;

    const HASH: &str = "abc123";

    #[test]
    fn ssh_and_https_remotes_give_the_same_link() {
        let want = "https://github.com/me/repo/blob/abc123/src/main.rs#L7";
        for remote in [
            "git@github.com:me/repo.git",
            "https://github.com/me/repo.git",
            "https://github.com/me/repo",
            "ssh://git@github.com/me/repo.git",
        ] {
            assert_eq!(
                remote_link(remote, HASH, "src/main.rs", 7).as_deref(),
                Some(want),
                "{remote}"
            );
        }
    }

    #[test]
    fn each_host_gets_its_own_url_shape() {
        assert_eq!(
            remote_link("git@gitlab.com:me/repo.git", HASH, "a.rs", 2).as_deref(),
            Some("https://gitlab.com/me/repo/-/blob/abc123/a.rs#L2"),
        );
        assert_eq!(
            remote_link("git@bitbucket.org:me/repo.git", HASH, "a.rs", 2).as_deref(),
            Some("https://bitbucket.org/me/repo/src/abc123/a.rs#lines-2"),
        );
    }

    #[test]
    fn an_unknown_host_gets_no_link_rather_than_a_wrong_one() {
        assert!(remote_link("git@git.example.com:me/repo.git", HASH, "a.rs", 1).is_none());
        assert!(remote_link("/srv/git/repo.git", HASH, "a.rs", 1).is_none());
        assert!(remote_link("git@github.com:", HASH, "a.rs", 1).is_none());
    }

    #[test]
    fn a_subgroup_path_is_kept_whole() {
        // GitLab nests groups, so the repo part can have more than one
        // slash in it and must not be truncated to the first two.
        assert_eq!(
            remote_link("git@gitlab.com:org/team/repo.git", HASH, "a.rs", 1).as_deref(),
            Some("https://gitlab.com/org/team/repo/-/blob/abc123/a.rs#L1"),
        );
    }
}

#[cfg(test)]
mod blame_tests {
    use super::parse_blame;

    /// Two commits over three lines, in git's own porcelain shape: the
    /// second mention of a sha carries no details, which is the whole
    /// reason the format is worth parsing rather than re-asking.
    const PORCELAIN: &str = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 1
author Ada Lovelace
author-time 1700000000
summary first pass
filename src/main.rs
\tfn main() {
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 2 2 1
author Grace Hopper
author-time 1700000000
summary second pass
filename src/main.rs
\t    work();
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3 3 1
filename src/main.rs
\t}
";

    #[test]
    fn each_line_gets_its_commit_and_each_commit_is_stored_once() {
        let blame = parse_blame(PORCELAIN);
        assert_eq!(blame.len(), 3, "one entry per line");
        assert_eq!(blame.commits.len(), 2, "the repeated sha was stored twice");

        assert_eq!(blame.line(0).unwrap().author, "Ada Lovelace");
        assert_eq!(blame.line(1).unwrap().author, "Grace Hopper");
        assert_eq!(
            blame.line(2).unwrap().author,
            "Ada Lovelace",
            "the third line repeats the first commit, whose details came earlier"
        );
        assert!(blame.line(3).is_none(), "there is no fourth line");
    }

    #[test]
    fn a_label_reads_as_one_line() {
        let blame = parse_blame(PORCELAIN);
        let label = blame.line(0).unwrap().label();
        assert!(label.starts_with("Ada Lovelace, "), "got {label:?}");
        assert!(label.ends_with("· first pass"), "got {label:?}");
    }

    #[test]
    fn a_line_in_no_commit_is_marked_rather_than_attributed() {
        let text = "\
0000000000000000000000000000000000000000 1 1 1
author Not Committed Yet
author-time 1700000000
summary Version of src/main.rs from src/main.rs
filename src/main.rs
\tfn new() {}
";
        let blame = parse_blame(text);
        let line = blame.line(0).unwrap();
        assert!(line.uncommitted);
        assert_eq!(
            line.label(),
            "not committed yet",
            "the placeholder author must not be shown as a person"
        );
    }

    #[test]
    fn content_that_looks_like_a_header_does_not_become_one() {
        // The content lines are tab-prefixed, which is what keeps a file
        // that itself contains blame output from confusing the parser.
        let text = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 1
author Ada Lovelace
author-time 1700000000
summary only one
filename notes.txt
\tcccccccccccccccccccccccccccccccccccccccc 9 9 1
";
        let blame = parse_blame(text);
        assert_eq!(blame.len(), 1, "the tabbed line is content, not a record");
        assert_eq!(blame.commits.len(), 1);
    }

    #[test]
    fn empty_output_is_an_empty_blame_not_a_panic() {
        let blame = parse_blame("");
        assert!(blame.is_empty());
        assert!(blame.line(0).is_none());
    }
}
