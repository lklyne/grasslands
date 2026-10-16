//! Tab titles: the file name, with just enough parent folders to tell apart
//! files that share it (`a/mod.rs`, `b/mod.rs`).

use std::path::{Component, Path};

/// One title per path, in order.
pub fn titles(paths: &[&Path]) -> Vec<String> {
    let components: Vec<Vec<String>> = paths
        .iter()
        .map(|path| {
            path.components()
                .filter_map(|component| match component {
                    Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    components
        .iter()
        .enumerate()
        .map(|(index, parts)| {
            let depth = (1..=parts.len().max(1))
                .find(|&depth| {
                    let mine = suffix(parts, depth);
                    components
                        .iter()
                        .enumerate()
                        .all(|(other, theirs)| other == index || suffix(theirs, depth) != mine)
                })
                .unwrap_or(parts.len());
            suffix(parts, depth).join("/")
        })
        .collect()
}

fn suffix(parts: &[String], depth: usize) -> &[String] {
    &parts[parts.len().saturating_sub(depth)..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(paths: &[&str]) -> Vec<String> {
        let paths: Vec<&Path> = paths.iter().map(Path::new).collect();
        titles(&paths)
    }

    #[test]
    fn unique_names_show_only_the_file_name() {
        assert_eq!(run(&["/r/src/a.rs", "/r/src/b.rs"]), ["a.rs", "b.rs"]);
    }

    #[test]
    fn same_names_add_the_parent_folder() {
        assert_eq!(
            run(&["/r/editor/mod.rs", "/r/sidebar/mod.rs", "/r/main.rs"]),
            ["editor/mod.rs", "sidebar/mod.rs", "main.rs"]
        );
    }

    #[test]
    fn shared_parents_go_up_until_they_differ() {
        assert_eq!(
            run(&["/r/one/src/lib.rs", "/r/two/src/lib.rs"]),
            ["one/src/lib.rs", "two/src/lib.rs"]
        );
    }

    #[test]
    fn a_shallower_path_gets_its_whole_path() {
        assert_eq!(run(&["/lib.rs", "/x/lib.rs"]), ["lib.rs", "x/lib.rs"]);
    }
}
