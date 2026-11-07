//! Lists the system's installed fonts that our text pipeline can actually
//! use. `ab_glyph` only parses plain TrueType glyf outlines from a single
//! font file — not font collections (`.ttc`, common on macOS for things
//! like Helvetica) and not CFF/PostScript outlines (common in `.otf`) —
//! so this filters down to `.ttf` files with a regular (non-bold,
//! non-italic) face before offering a family as a choice.

use std::collections::HashMap;
use std::path::PathBuf;

/// A font family the text tool can use, with the file for its regular
/// face. Bold is always synthesized (see `document::draw_text_element`),
/// so no separate bold path is tracked here.
#[derive(Debug, Clone)]
pub struct FontChoice {
    pub family: String,
    pub path: PathBuf,
}

/// Enumerates installed system fonts and returns one [`FontChoice`] per
/// family that has a usable regular `.ttf` face, sorted by family name.
/// Scans the filesystem, so callers should cache the result rather than
/// calling this on every render.
pub fn available_fonts() -> Vec<FontChoice> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    let mut by_family: HashMap<String, PathBuf> = HashMap::new();
    for face in db.faces() {
        let Some((family, _)) = face.families.first() else {
            continue;
        };
        // Leading-dot families are macOS's private system UI fonts
        // (".SF Arabic", ".New York", ...) — not meant to be user-facing.
        if family.starts_with('.') {
            continue;
        }
        let fontdb::Source::File(path) = &face.source else {
            continue;
        };
        let is_ttf = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf"));
        let is_regular = face.style == fontdb::Style::Normal && face.weight == fontdb::Weight::NORMAL;
        if is_ttf && is_regular {
            by_family.entry(family.clone()).or_insert_with(|| path.clone());
        }
    }

    let mut choices: Vec<FontChoice> =
        by_family.into_iter().map(|(family, path)| FontChoice { family, path }).collect();
    choices.sort_by(|a, b| a.family.cmp(&b.family));
    choices
}
