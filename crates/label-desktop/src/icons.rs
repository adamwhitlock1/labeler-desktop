//! Bundled icon set (Heroicons, solid/24px, MIT licensed) for the
//! Builder's icon picker. Embedded into the binary at compile time, then
//! extracted on demand to a cache file on disk — the rest of the app only
//! ever deals with [`label_render::document::ElementKind::Image`] paths
//! on real files, so this is the only place that knows the icons are
//! bundled rather than user-supplied.

use std::path::PathBuf;

#[derive(rust_embed::RustEmbed)]
#[folder = "assets/icons"]
struct IconAssets;

/// All bundled icon names (without the `.svg` extension), sorted.
pub fn icon_names() -> Vec<String> {
    let mut names: Vec<String> = IconAssets::iter()
        .filter_map(|f| f.strip_suffix(".svg").map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Raw SVG bytes for `name`, straight out of the embedded binary — for
/// drawing an icon in the UI itself (toolbar buttons, the icon picker's own
/// search results). Unlike [`icon_cache_path`], this never touches disk:
/// there's no need for a stable file path when the icon is only ever handed
/// to GPUI's own SVG renderer, not stored as an element on the label.
pub fn icon_bytes(name: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
    IconAssets::get(&format!("{name}.svg")).map(|f| f.data)
}

/// Extracts `name`'s SVG to a stable cache file on disk (writing it only
/// once) and returns that path, ready to use as an
/// [`label_render::document::ElementKind::Image`] path like any other
/// image file.
pub fn icon_cache_path(name: &str) -> Option<PathBuf> {
    let filename = format!("{name}.svg");
    let asset = IconAssets::get(&filename)?;

    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("labeler-desktop")
        .join("icons");
    std::fs::create_dir_all(&cache_dir).ok()?;

    let path = cache_dir.join(&filename);
    if !path.exists() {
        std::fs::write(&path, asset.data.as_ref()).ok()?;
    }
    Some(path)
}
