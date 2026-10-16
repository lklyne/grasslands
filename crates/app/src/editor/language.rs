//! Which tree-sitter grammar highlights a file.

use std::path::Path;

/// The GPUI Kit language name for `path`, or `"text"` for no highlighting.
pub fn for_path(path: &Path) -> &'static str {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match file_name {
        "Makefile" | "makefile" | "GNUmakefile" => return "make",
        "Cargo.lock" => return "toml",
        _ => {}
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "md" | "mdx" | "markdown" => "markdown",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "sh" | "bash" | "zsh" => "bash",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "lua" => "lua",
        "swift" => "swift",
        "yaml" | "yml" => "yaml",
        "zig" => "zig",
        "diff" | "patch" => "diff",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_files_highlight_as_rust() {
        assert_eq!(for_path(Path::new("src/main.rs")), "rust");
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
}
