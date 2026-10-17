//! Colours. A plain struct for now; M3 replaces the hardcoded values with a
//! TOML file read from the config directory, same fields.

use lithe_gpu::Rgba;
use lithe_syntax::Kind;

#[allow(dead_code)] // `error` lands in M6, with diagnostics
pub struct Theme {
    pub bg: Rgba,
    pub fg: Rgba,
    pub gutter_bg: Rgba,
    pub gutter_fg: Rgba,
    pub gutter_fg_active: Rgba,
    pub cursor_line: Rgba,
    pub cursor: Rgba,
    pub selection: Rgba,
    /// Body text in the Markdown preview. A shade quieter than the
    /// editor's own: code is read a line at a time and wants contrast,
    /// prose is read by the paragraph and does not.
    pub preview_fg: Rgba,
    pub status_bg: Rgba,
    pub status_fg: Rgba,
    pub error: Rgba,
    pub diagnostic_warning: Rgba,
    pub diagnostic_hint: Rgba,
    pub vcs_added: Rgba,
    pub vcs_modified: Rgba,
    pub vcs_removed: Rgba,
    pub diff_added_bg: Rgba,
    pub diff_removed_bg: Rgba,
    /// Chrome: file tree, tab strip, picker panel.
    pub panel_bg: Rgba,
    pub panel_active: Rgba,
    /// What sits under the pointer. Drawn over whatever is behind it,
    /// so it reads the same on a selected row as on a bare one.
    pub hover: Rgba,
    pub tree_dir: Rgba,
    pub tree_file: Rgba,
    /// Dims the editor behind the picker.
    pub scrim: Rgba,
    pub scrollbar_track: Rgba,
    pub scrollbar_thumb: Rgba,
    /// Behind an in-buffer search match.
    pub match_bg: Rgba,
    pub syntax: SyntaxColors,
}

/// One colour per [`Kind`]. Keeping the set small is deliberate: a theme is
/// a short list rather than a hundred scope selectors.
#[allow(dead_code)]
pub struct SyntaxColors {
    pub keyword: Rgba,
    pub function: Rgba,
    pub type_name: Rgba,
    pub constructor: Rgba,
    pub string: Rgba,
    pub escape: Rgba,
    pub number: Rgba,
    pub boolean: Rgba,
    pub comment: Rgba,
    pub constant: Rgba,
    pub variable: Rgba,
    pub property: Rgba,
    pub parameter: Rgba,
    pub operator: Rgba,
    pub punctuation: Rgba,
    pub attribute: Rgba,
    pub namespace: Rgba,
}

impl Theme {
    /// Colour for a highlight kind; plain foreground where there is none.
    pub fn colour(&self, kind: Option<Kind>) -> Rgba {
        let s = &self.syntax;
        match kind {
            None => self.fg,
            Some(Kind::Keyword) => s.keyword,
            Some(Kind::Function) => s.function,
            Some(Kind::Type) => s.type_name,
            Some(Kind::Constructor) => s.constructor,
            Some(Kind::String) => s.string,
            Some(Kind::Escape) => s.escape,
            Some(Kind::Number) => s.number,
            Some(Kind::Boolean) => s.boolean,
            Some(Kind::Comment) => s.comment,
            Some(Kind::Constant) => s.constant,
            Some(Kind::Variable) => s.variable,
            Some(Kind::Property) => s.property,
            Some(Kind::Parameter) => s.parameter,
            Some(Kind::Operator) => s.operator,
            Some(Kind::Punctuation) => s.punctuation,
            Some(Kind::Attribute) => s.attribute,
            Some(Kind::Namespace) => s.namespace,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Rgba::hex(0x14161B),
            fg: Rgba::hex(0xC8CCD4),
            gutter_bg: Rgba::hex(0x14161B),
            gutter_fg: Rgba::hex(0x3C4250),
            gutter_fg_active: Rgba::hex(0x8A93A5),
            cursor_line: Rgba::hex(0x1A1D25),
            cursor: Rgba::hex(0x6EA8FE),
            // Drawn behind the glyphs, so it has to stay dark enough to read
            // through.
            selection: Rgba::hex(0x2A4A6B),
            preview_fg: Rgba::hex(0xB6BCC8),
            status_bg: Rgba::hex(0x1A1D24),
            status_fg: Rgba::hex(0x7A8495),
            error: Rgba::hex(0xE06C75),
            diagnostic_warning: Rgba::hex(0xD79A66),
            diagnostic_hint: Rgba::hex(0x5FB3C2),
            vcs_added: Rgba::hex(0x5FA86B),
            vcs_modified: Rgba::hex(0x4E7FC0),
            vcs_removed: Rgba::hex(0xB4555E),
            diff_added_bg: Rgba::hex(0x182A1C),
            diff_removed_bg: Rgba::hex(0x2C1A1D),
            // Recessed, and visibly so. At a couple of points away
            // from the editor's own background the sidebar and the
            // tab strip did not read as panels at all — the window
            // looked like one flat sheet with text in different
            // places on it.
            panel_bg: Rgba::hex(0x0C0E13),
            panel_active: Rgba::hex(0x232832),
            // Deliberately faint. Hover is an answer to "is this a
            // thing I can click", not a selection.
            hover: Rgba::hex(0xFFFFFF).alpha(0.06),
            tree_dir: Rgba::hex(0x9DB2CE),
            tree_file: Rgba::hex(0x868E9E),
            scrim: Rgba::hex(0x05070A).alpha(0.62),
            scrollbar_track: Rgba::hex(0x14161B),
            scrollbar_thumb: Rgba::hex(0x3A4150),
            match_bg: Rgba::hex(0x5A4A1E),
            syntax: SyntaxColors {
                keyword: Rgba::hex(0xC18FE0),
                function: Rgba::hex(0x6EA8FE),
                type_name: Rgba::hex(0xE3C07B),
                constructor: Rgba::hex(0xE3C07B),
                string: Rgba::hex(0x8FBF7F),
                escape: Rgba::hex(0x5FB3C2),
                number: Rgba::hex(0xD79A66),
                boolean: Rgba::hex(0xD79A66),
                comment: Rgba::hex(0x5B6475),
                constant: Rgba::hex(0xD79A66),
                variable: Rgba::hex(0xC8CCD4),
                property: Rgba::hex(0xE0858C),
                parameter: Rgba::hex(0xB7BECC),
                operator: Rgba::hex(0x5FB3C2),
                punctuation: Rgba::hex(0x79818F),
                attribute: Rgba::hex(0xE3C07B),
                namespace: Rgba::hex(0x5FC2AE),
            },
        }
    }
}
