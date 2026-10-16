//! Which tree-sitter grammar highlights a file.

use std::path::Path;

/// Whole file names that decide the language on their own.
const FILE_NAMES: &[(&str, &str)] = &[
    ("Makefile", "make"),
    ("makefile", "make"),
    ("GNUmakefile", "make"),
    ("CMakeLists.txt", "cmake"),
    ("Cargo.lock", "toml"),
];

/// Lower-case extensions and the GPUI Kit language they map to.
const EXTENSIONS: &[(&[&str], &str)] = &[
    (&["rs"], "rust"),
    (&["toml"], "toml"),
    (&["json", "jsonc"], "json"),
    (&["md", "mdx", "markdown"], "markdown"),
    (&["js", "mjs", "cjs", "jsx"], "javascript"),
    (&["ts", "mts", "cts"], "typescript"),
    (&["tsx"], "tsx"),
    (&["py"], "python"),
    (&["rb"], "ruby"),
    (&["go"], "go"),
    (&["sh", "bash", "zsh"], "bash"),
    (&["c", "h"], "c"),
    (&["cc", "cpp", "cxx", "hpp", "hh"], "cpp"),
    (&["cs"], "csharp"),
    (&["css", "scss"], "css"),
    (&["html", "htm"], "html"),
    (&["java"], "java"),
    (&["kt", "kts"], "kotlin"),
    (&["lua"], "lua"),
    (&["swift"], "swift"),
    (&["yaml", "yml"], "yaml"),
    (&["zig"], "zig"),
    (&["diff", "patch"], "diff"),
    (&["sql"], "sql"),
    (&["php"], "php"),
    (&["proto"], "proto"),
    (&["scala", "sc"], "scala"),
    (&["svelte"], "svelte"),
    (&["graphql", "gql"], "graphql"),
    (&["ex", "exs"], "elixir"),
    (&["astro"], "astro"),
    (&["erb"], "erb"),
    (&["ejs"], "ejs"),
    (&["cmake"], "cmake"),
];

/// The GPUI Kit language name for `path`, or `"text"` for no highlighting.
pub fn for_path(path: &Path) -> &'static str {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if let Some((_, language)) = FILE_NAMES.iter().find(|(name, _)| *name == file_name) {
        return language;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .find(|(extensions, _)| extensions.contains(&extension.as_str()))
        .map_or("text", |(_, language)| language)
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::highlighter::Language;

    use super::*;

    #[test]
    fn rust_files_highlight_as_rust() {
        assert_eq!(for_path(Path::new("src/main.rs")), "rust");
    }

    #[test]
    fn sql_files_highlight_as_sql() {
        assert_eq!(for_path(Path::new("schema.sql")), "sql");
    }

    #[test]
    fn makefile_is_detected_by_name() {
        assert_eq!(for_path(Path::new("dir/Makefile")), "make");
    }

    #[test]
    fn extension_match_ignores_case() {
        assert_eq!(for_path(Path::new("README.MD")), "markdown");
    }

    #[test]
    fn unknown_extension_is_plain_text() {
        assert_eq!(for_path(Path::new("notes.xyz")), "text");
    }

    /// A grammar left out of gpui-kit's features would silently fall back
    /// to plain text.
    #[test]
    fn every_mapped_language_has_a_grammar() {
        let languages = FILE_NAMES
            .iter()
            .map(|(_, language)| language)
            .chain(EXTENSIONS.iter().map(|(_, language)| language));
        for language in languages {
            assert_ne!(
                Language::from_str(language),
                Language::Plain,
                "no grammar for {language:?}"
            );
        }
    }
}
