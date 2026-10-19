//! Tab titles: the file name, with just enough parent folders to tell apart
//! files that share it (`a/mod.rs`, `b/mod.rs`). A file on a saved machine
//! also names the machine (`main.rs — dockerbox`), like the window title.

use std::path::{Component, Path};

use crate::state::MachineId;

/// One title per `(machine, path)`, in order. Paths are told apart only from
/// others on the same machine; the machine's label tells the rest apart.
pub fn titles_on(
    files: &[(&MachineId, &Path)],
    label: impl Fn(&MachineId) -> String,
) -> Vec<String> {
    let mut out = vec![String::new(); files.len()];
    let mut machines: Vec<&MachineId> = Vec::new();
    for (machine, _) in files {
        if !machines.contains(machine) {
            machines.push(machine);
        }
    }
    for machine in machines {
        let (indexes, paths): (Vec<usize>, Vec<&Path>) = files
            .iter()
            .enumerate()
            .filter(|(_, (m, _))| *m == machine)
            .map(|(index, (_, path))| (index, *path))
            .unzip();
        let suffix = (!machine.is_local()).then(|| format!(" — {}", label(machine)));
        for (index, title) in indexes.into_iter().zip(titles(&paths)) {
            out[index] = match &suffix {
                Some(suffix) => title + suffix,
                None => title,
            };
        }
    }
    out
}

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

    fn remote(id: &str) -> MachineId {
        MachineId::Remote(id.into())
    }

    #[test]
    fn a_remote_file_names_its_machine() {
        let box_ = remote("p1");
        let titles = titles_on(&[(&box_, Path::new("/r/main.rs"))], |_| "dockerbox".into());
        assert_eq!(titles, ["main.rs — dockerbox"]);
    }

    #[test]
    fn the_same_path_on_two_machines_differs_only_by_machine() {
        let (local, box_) = (MachineId::Local, remote("p1"));
        let path = Path::new("/r/src/main.rs");
        let titles = titles_on(&[(&local, path), (&box_, path)], |_| "dockerbox".into());
        assert_eq!(titles, ["main.rs", "main.rs — dockerbox"]);
    }

    #[test]
    fn same_names_on_one_machine_still_add_folders() {
        let box_ = remote("p1");
        let titles = titles_on(
            &[
                (&box_, Path::new("/r/a/mod.rs")),
                (&MachineId::Local, Path::new("/r/c/mod.rs")),
                (&box_, Path::new("/r/b/mod.rs")),
            ],
            |_| "box".into(),
        );
        assert_eq!(titles, ["a/mod.rs — box", "mod.rs", "b/mod.rs — box"]);
    }

    #[test]
    fn a_shallower_path_gets_its_whole_path() {
        assert_eq!(run(&["/lib.rs", "/x/lib.rs"]), ["lib.rs", "x/lib.rs"]);
    }
}
