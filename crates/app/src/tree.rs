//! The file tree.
//!
//! Directories are read only when they are expanded, and the flattened row
//! list is rebuilt from the expanded set. Opening a folder therefore reads
//! exactly one directory, not the project — which is invariant 6, and the
//! difference between opening a monorepo instantly and hanging for a minute.
//!
//! Listing goes through `ignore`, so `.gitignore`, `.ignore` and global git
//! excludes are all respected for free, and `target/` or `node_modules/`
//! never show up.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// What an inline edit in the tree will do when it is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Editing {
    /// Create a file in this directory.
    NewFile(PathBuf),
    /// Create a directory in this directory.
    NewFolder(PathBuf),
    /// Rename this path.
    Rename(PathBuf),
}

impl Editing {
    /// Where the edit row appears: inside the parent directory.
    pub fn parent(&self) -> &Path {
        match self {
            Editing::NewFile(dir) | Editing::NewFolder(dir) => dir,
            Editing::Rename(path) => path.parent().unwrap_or(Path::new("/")),
        }
    }
}

/// What `finish_edit` did.
#[derive(Debug)]
pub struct Made {
    /// Set only by a rename: the path it used to be at.
    pub from: Option<PathBuf>,
    pub to: PathBuf,
}

pub struct Tree {
    pub root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    pub rows: Vec<Row>,
    pub scroll: usize,
    /// An in-place name being typed, and what it is for.
    pub editing: Option<(Editing, String)>,
    /// Which row the name is being typed at, set by `rebuild`.
    ///
    /// The row is a real entry in `rows`, inserted where the new file
    /// will land. Drawing and hit-testing then need no notion of it at
    /// all: it is a row like every other, and the one place that cares is
    /// the one line that draws its label.
    pub edit_at: Option<usize>,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            expanded: BTreeSet::from([root.clone()]),
            root,
            rows: Vec::new(),
            scroll: 0,
            editing: None,
            edit_at: None,
        };
        tree.rebuild();
        tree
    }

    /// Expands or collapses a directory and rebuilds the visible rows.
    pub fn toggle(&mut self, path: &Path) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
    }

    /// Expands every directory on the way to `path`, so revealing a file
    /// opened by other means works.
    pub fn reveal(&mut self, path: &Path) {
        let mut cursor = path.parent();
        while let Some(dir) = cursor {
            self.expanded.insert(dir.to_path_buf());
            if dir == self.root {
                break;
            }
            cursor = dir.parent();
        }
        self.rebuild();
    }

    /// Starts naming something in place. The directory it lands in is
    /// expanded first, so the row being typed into is visible.
    pub fn begin_edit(&mut self, editing: Editing) {
        let seed = match &editing {
            Editing::Rename(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.expanded.insert(editing.parent().to_path_buf());
        self.editing = Some((editing, seed));
        self.rebuild();
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
        self.rebuild();
    }

    /// The name typed so far, if something is being named.
    pub fn typed(&self) -> Option<&str> {
        self.editing.as_ref().map(|(_, name)| name.as_str())
    }

    /// Adds to the name being typed.
    pub fn push_typed(&mut self, text: &str) {
        if let Some((_, name)) = self.editing.as_mut() {
            name.push_str(text);
        }
    }

    pub fn pop_typed(&mut self) {
        if let Some((_, name)) = self.editing.as_mut() {
            name.pop();
        }
    }

    /// Carries out the edit being typed.
    ///
    /// Returns what it did: the path it produced, and for a rename the
    /// path it came from, so an open buffer can follow it.
    pub fn finish_edit(&mut self) -> Result<Option<Made>, std::io::Error> {
        let Some((editing, name)) = self.editing.take() else {
            return Ok(None);
        };
        let name = name.trim().to_string();
        self.rebuild();
        if name.is_empty() {
            return Ok(None);
        }

        // A name, not a path: `../x` or `/x` from a tree row is more
        // likely a typo than a request, and silently writing outside the
        // directory you clicked is the wrong way to find out.
        if Path::new(&name).components().count() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a name cannot contain a path separator",
            ));
        }

        let mut from = None;
        let made = match &editing {
            Editing::NewFile(dir) => {
                let path = dir.join(&name);
                if path.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("{name} already exists"),
                    ));
                }
                std::fs::write(&path, "")?;
                path
            }
            Editing::NewFolder(dir) => {
                let path = dir.join(&name);
                std::fs::create_dir_all(&path)?;
                self.expanded.insert(path.clone());
                path
            }
            Editing::Rename(old) => {
                let path = old.parent().unwrap_or(Path::new("/")).join(&name);
                if path != *old {
                    if path.exists() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            format!("{name} already exists"),
                        ));
                    }
                    std::fs::rename(old, &path)?;
                    // A renamed directory keeps its open state.
                    if self.expanded.remove(old) {
                        self.expanded.insert(path.clone());
                    }
                    from = Some(old.clone());
                }
                path
            }
        };
        self.rebuild();
        Ok(Some(Made { from, to: made }))
    }

    pub fn rebuild(&mut self) {
        self.rows.clear();
        let root = self.root.clone();
        self.push_children(&root, 0);
        self.edit_at = self.place_edit();
    }

    /// Puts the row being typed into where its result will appear.
    fn place_edit(&mut self) -> Option<usize> {
        let (editing, _) = self.editing.as_ref()?;
        let parent = editing.parent().to_path_buf();
        if let Editing::Rename(path) = editing {
            return self.rows.iter().position(|r| r.path == *path);
        }

        // A new entry is typed after the last of its directory's
        // children, which is where sorting will not have put it — but
        // where the eye is already looking.
        let (start, depth) = if parent == self.root {
            (0, 0)
        } else {
            let at = self.rows.iter().position(|r| r.path == parent)?;
            (at + 1, self.rows[at].depth + 1)
        };
        let end = self.rows[start..]
            .iter()
            .position(|r| r.depth < depth)
            .map(|i| start + i)
            .unwrap_or(self.rows.len());
        self.rows.insert(
            end,
            Row {
                path: parent,
                name: String::new(),
                depth,
                is_dir: matches!(editing, Editing::NewFolder(_)),
                expanded: false,
            },
        );
        Some(end)
    }

    fn push_children(&mut self, dir: &Path, depth: usize) {
        for (path, is_dir) in children(dir) {
            let expanded = is_dir && self.expanded.contains(&path);
            self.rows.push(Row {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                depth,
                is_dir,
                expanded,
                path: path.clone(),
            });
            if expanded {
                self.push_children(&path, depth + 1);
            }
        }
    }
}

/// One directory's worth of entries, gitignore-filtered, directories first
/// then files, each alphabetical and case-insensitive.
fn children(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut out: Vec<(PathBuf, bool)> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .parents(true)
        // A .gitignore means what it says whether or not `git init` has
        // been run. ripgrep defaults the other way because it is a search
        // tool; for a file tree, honouring the file the user wrote is less
        // surprising.
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        // depth 0 is `dir` itself.
        .filter(|entry| entry.depth() == 1)
        .map(|entry| {
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            (entry.path().to_path_buf(), is_dir)
        })
        .collect();

    out.sort_by(|(a_path, a_dir), (b_path, b_dir)| {
        b_dir.cmp(a_dir).then_with(|| {
            let a = a_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let b = b_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            a.cmp(&b)
        })
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// Builds a small tree on disk and hands back its root.
    fn fixture() -> TempDir {
        let dir = TempDir::new();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/deep/mod.rs"), "\n").unwrap();
        std::fs::write(dir.path().join("target/junk.o"), "\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        dir
    }

    #[test]
    fn only_the_root_is_read_until_something_is_expanded() {
        let dir = fixture();
        let tree = Tree::new(dir.path().to_path_buf());
        let names: Vec<&str> = tree.rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"src"), "got {names:?}");
        assert!(names.contains(&"Cargo.toml"), "got {names:?}");
        assert!(
            !names.contains(&"main.rs"),
            "an unexpanded directory must not be read: {names:?}"
        );
    }

    #[test]
    fn gitignored_directories_are_hidden() {
        let dir = fixture();
        let tree = Tree::new(dir.path().to_path_buf());
        let names: Vec<&str> = tree.rows.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !names.contains(&"target"),
            "target/ is gitignored: {names:?}"
        );
        assert!(
            !names.contains(&".gitignore"),
            "dotfiles are hidden: {names:?}"
        );
    }

    #[test]
    fn directories_sort_before_files() {
        let dir = fixture();
        let tree = Tree::new(dir.path().to_path_buf());
        let first_file = tree.rows.iter().position(|r| !r.is_dir);
        let last_dir = tree.rows.iter().rposition(|r| r.is_dir);
        if let (Some(file), Some(d)) = (first_file, last_dir) {
            assert!(d < file, "a directory sorted after a file");
        }
    }

    #[test]
    fn expanding_reveals_children_and_collapsing_hides_them() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let src = dir.path().join("src");

        tree.toggle(&src);
        let names: Vec<&str> = tree.rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"main.rs"), "got {names:?}");
        assert!(
            !names.contains(&"mod.rs"),
            "nested directories stay closed: {names:?}"
        );

        tree.toggle(&src);
        let names: Vec<&str> = tree.rows.iter().map(|r| r.name.as_str()).collect();
        assert!(!names.contains(&"main.rs"), "collapse should hide children");
    }

    #[test]
    fn a_new_file_is_typed_at_a_row_inside_its_directory() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let src = dir.path().join("src");
        tree.toggle(&src);

        let before = tree.rows.len();
        tree.begin_edit(Editing::NewFile(src.clone()));
        assert_eq!(tree.rows.len(), before + 1, "the edit row is a real row");

        let at = tree.edit_at.expect("no edit row");
        // Inside src, which means after src's own row and before
        // anything shallower than its children.
        let parent = tree.rows.iter().position(|r| r.path == src).unwrap();
        assert!(at > parent, "the row landed outside its directory");
        assert_eq!(tree.rows[at].depth, tree.rows[parent].depth + 1);
    }

    #[test]
    fn a_new_file_is_created_and_reported() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        tree.begin_edit(Editing::NewFile(dir.path().to_path_buf()));
        tree.push_typed("notes.md");
        let made = tree.finish_edit().unwrap().expect("nothing was made");

        assert_eq!(made.to, dir.path().join("notes.md"));
        assert!(made.from.is_none(), "a creation has nothing to come from");
        assert!(made.to.is_file());
        assert!(tree.rows.iter().any(|r| r.name == "notes.md"));
        assert!(tree.edit_at.is_none(), "the edit row is gone");
    }

    #[test]
    fn a_new_folder_is_created_expanded() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        tree.begin_edit(Editing::NewFolder(dir.path().to_path_buf()));
        tree.push_typed("docs");
        let made = tree.finish_edit().unwrap().unwrap();

        assert!(made.to.is_dir());
        let row = tree.rows.iter().find(|r| r.name == "docs").unwrap();
        assert!(row.expanded, "a folder you just made should be open");
    }

    #[test]
    fn renaming_reports_both_names_so_an_open_buffer_can_follow() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let from = dir.path().join("Cargo.toml");

        tree.begin_edit(Editing::Rename(from.clone()));
        assert_eq!(tree.typed(), Some("Cargo.toml"), "seeded with the old name");
        for _ in 0..10 {
            tree.pop_typed();
        }
        tree.push_typed("Cargo.toml.bak");

        let made = tree.finish_edit().unwrap().unwrap();
        assert_eq!(made.from, Some(from.clone()));
        assert_eq!(made.to, dir.path().join("Cargo.toml.bak"));
        assert!(!from.exists() && made.to.exists());
    }

    #[test]
    fn an_empty_name_makes_nothing() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        tree.begin_edit(Editing::NewFile(dir.path().to_path_buf()));
        assert!(tree.finish_edit().unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
    }

    #[test]
    fn a_name_cannot_escape_the_directory_it_was_typed_in() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        tree.begin_edit(Editing::NewFile(dir.path().join("src")));
        tree.push_typed("../escaped.rs");

        let error = tree
            .finish_edit()
            .expect_err("a path was accepted as a name");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!dir.path().join("escaped.rs").exists());
    }

    #[test]
    fn a_name_that_is_taken_is_refused_rather_than_overwriting() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        tree.begin_edit(Editing::NewFile(dir.path().to_path_buf()));
        tree.push_typed("Cargo.toml");

        let error = tree
            .finish_edit()
            .expect_err("an existing file was clobbered");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap(),
            "[package]\n",
            "the file on disk was changed"
        );
    }

    #[test]
    fn cancelling_leaves_the_tree_as_it_was() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let before = tree.rows.len();
        tree.begin_edit(Editing::NewFile(dir.path().to_path_buf()));
        tree.push_typed("gone.txt");
        tree.cancel_edit();

        assert_eq!(tree.rows.len(), before);
        assert!(tree.edit_at.is_none());
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn reveal_opens_every_directory_on_the_path() {
        let dir = fixture();
        let mut tree = Tree::new(dir.path().to_path_buf());
        tree.reveal(&dir.path().join("src/deep/mod.rs"));
        let names: Vec<&str> = tree.rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"mod.rs"), "got {names:?}");
    }
}
