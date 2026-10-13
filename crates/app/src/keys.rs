//! What this platform calls things.
//!
//! The editor shows its shortcuts in three places — the landing page, the
//! right-click menus and the status bar — and the modifier they hang off
//! has a different name on each platform: `cmd` on macOS, `ctrl` on
//! Windows and Linux. A hint that names the wrong one is worse than no
//! hint, so the name is chosen once here instead of at fifteen call sites.
//!
//! [`key!`] builds the hints at compile time, which is what lets them stay
//! `&'static str` and cost nothing.

/// The modifier's name, for a hint that has to be built at runtime.
#[cfg(target_os = "macos")]
pub const MOD: &str = "cmd";
#[cfg(not(target_os = "macos"))]
pub const MOD: &str = "ctrl";

/// A shortcut hint: `key!(" shift V")` is "cmd shift V" on macOS and
/// "ctrl shift V" everywhere else.
///
/// Each literal given is preceded by the modifier's name, so a hint that
/// mentions it twice is written as two: `key!("+P  open a file  ",
/// "+shift+P  commands")`.
#[cfg(target_os = "macos")]
macro_rules! key {
    ($($rest:literal),+) => { concat!($("cmd", $rest),+) };
}

#[cfg(not(target_os = "macos"))]
macro_rules! key {
    ($($rest:literal),+) => { concat!($("ctrl", $rest),+) };
}

/// What the place a trashed file goes is called. On Windows it is a
/// directory of lithe's own rather than the Recycle Bin — see
/// `trash_dir` — so saying "Trash" there would name the wrong thing.
#[cfg(target_os = "windows")]
pub const TRASH: &str = "lithe's trash";
#[cfg(not(target_os = "windows"))]
pub const TRASH: &str = "Trash";

/// What the system's file browser is called, for the menu entry that
/// opens it.
#[cfg(target_os = "macos")]
pub const REVEAL: &str = "Reveal in Finder";
#[cfg(target_os = "windows")]
pub const REVEAL: &str = "Reveal in Explorer";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const REVEAL: &str = "Reveal in File Manager";
