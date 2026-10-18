//! The `Host` entries of an OpenSSH client config, for suggesting machines to
//! add. Only what a suggestion shows is read: each concrete alias and its
//! `HostName`, `User` and `Port`, resolved the way ssh does (the first value
//! from any block whose patterns match wins, so `Host *` defaults apply).
//! Wildcard and negated patterns aren't hosts; `Match` blocks are skipped.

use std::path::{Path, PathBuf};

/// `Include` nesting ssh itself allows.
const MAX_INCLUDE_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
}

impl SshHost {
    /// `user@hostname[:port]`, what the alias connects to; the alias itself
    /// stands in for a missing `HostName`.
    pub fn destination(&self) -> String {
        let host = self.hostname.as_deref().unwrap_or(&self.alias);
        let mut text = match &self.user {
            Some(user) => format!("{user}@{host}"),
            None => host.to_owned(),
        };
        if let Some(port) = self.port.filter(|&port| port != 22) {
            text.push_str(&format!(":{port}"));
        }
        text
    }
}

/// `~/.ssh/config`.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".ssh/config"))
}

/// The hosts `path` names, in file order. Relative `Include` paths resolve
/// against `path`'s directory (`~/.ssh` for the user's config). A missing or
/// unreadable file has no hosts.
pub fn hosts(path: &Path) -> Vec<SshHost> {
    let base = path.parent().unwrap_or(Path::new("."));
    let mut blocks = vec![Block::all()];
    read_file(path, base, 0, &mut blocks);
    resolve(&blocks)
}

/// The hosts config text names, with relative `Include` paths under `base`.
pub fn from_text(text: &str, base: &Path) -> Vec<SshHost> {
    let mut blocks = vec![Block::all()];
    parse(text, base, 0, &mut blocks);
    resolve(&blocks)
}

/// One `Host` block's patterns and the settings under it. Settings before the
/// first `Host` apply to every host; a `Match` block's apply to none here.
#[derive(Debug, Default)]
struct Block {
    /// `None` for a `Match` block.
    patterns: Option<Vec<String>>,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
}

impl Block {
    fn all() -> Self {
        Self {
            patterns: Some(vec!["*".to_owned()]),
            ..Self::default()
        }
    }

    fn matches(&self, alias: &str) -> bool {
        let Some(patterns) = &self.patterns else {
            return false;
        };
        let mut matched = false;
        for pattern in patterns {
            match pattern.strip_prefix('!') {
                Some(negated) if wildcard_match(negated, alias) => return false,
                Some(_) => {}
                None => matched |= wildcard_match(pattern, alias),
            }
        }
        matched
    }
}

fn read_file(path: &Path, base: &Path, depth: usize, blocks: &mut Vec<Block>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, base, depth, blocks),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::debug!("ssh config {}: {error}", path.display()),
    }
}

fn parse(text: &str, base: &Path, depth: usize, blocks: &mut Vec<Block>) {
    for line in text.lines() {
        let Some((keyword, args)) = directive(line) else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "host" => blocks.push(Block {
                patterns: Some(args),
                ..Block::default()
            }),
            "match" => blocks.push(Block::default()),
            "include" if depth < MAX_INCLUDE_DEPTH => {
                for pattern in &args {
                    for file in include_files(pattern, base) {
                        read_file(&file, base, depth + 1, blocks);
                    }
                }
            }
            setting => {
                let (Some(block), Some(value)) = (blocks.last_mut(), args.into_iter().next())
                else {
                    continue;
                };
                match setting {
                    "hostname" => {
                        block.hostname.get_or_insert(value);
                    }
                    "user" => {
                        block.user.get_or_insert(value);
                    }
                    "port" => {
                        if let (None, Ok(port)) = (block.port, value.parse()) {
                            block.port = Some(port);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// A line's keyword and arguments: `Keyword arg…` or `Keyword=arg…`, with
/// double-quoted arguments kept whole. `None` for blanks and comments.
fn directive(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(split);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    Some((keyword.to_owned(), arguments(rest)))
}

fn arguments(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// The files an `Include` argument names: `~/` is the home directory, a
/// relative path is under `base`, and the file name may hold `*` and `?`.
fn include_files(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => return Vec::new(),
        },
        None => base.join(pattern),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| wildcard_match(&name, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    // ssh reads glob matches in sorted order.
    files.sort();
    files
}

/// ssh's pattern match: `*` is any run of characters, `?` any one.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c.eq_ignore_ascii_case(&text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

fn is_concrete(pattern: &str) -> bool {
    !pattern.starts_with('!') && !pattern.contains(['*', '?'])
}

fn resolve(blocks: &[Block]) -> Vec<SshHost> {
    let mut aliases: Vec<&str> = Vec::new();
    for pattern in blocks.iter().filter_map(|b| b.patterns.as_ref()).flatten() {
        if is_concrete(pattern) && !aliases.contains(&pattern.as_str()) {
            aliases.push(pattern);
        }
    }
    aliases
        .into_iter()
        .map(|alias| {
            let mut host = SshHost {
                alias: alias.to_owned(),
                hostname: None,
                user: None,
                port: None,
            };
            for block in blocks.iter().filter(|b| b.matches(alias)) {
                if host.hostname.is_none() {
                    host.hostname = block.hostname.as_ref().map(|h| h.replace("%h", alias));
                }
                host.user = host.user.take().or_else(|| block.user.clone());
                host.port = host.port.or(block.port);
            }
            host
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Vec<SshHost> {
        from_text(text, Path::new("/nonexistent"))
    }

    fn host(alias: &str, hostname: Option<&str>, user: Option<&str>, port: Option<u16>) -> SshHost {
        SshHost {
            alias: alias.into(),
            hostname: hostname.map(Into::into),
            user: user.map(Into::into),
            port,
        }
    }

    /// A scratch directory of its own for a test that reads files.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "grasslands-ssh-config-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config.d")).unwrap();
        dir
    }

    const FIXTURE: &str = r#"
# personal
Host gpu-lab
    HostName 10.0.4.21
    User lyle

Host gpu-box-2 gpu2
  Hostname=gpu2.internal
  user lyle
  Port 2200

Host *.internal !bastion bastion
    User ops

Match host foo
    User nobody

Host *
    User fallback
    Port 22
"#;

    #[test]
    fn hosts_come_in_file_order_without_patterns() {
        let aliases: Vec<String> = parse_text(FIXTURE).into_iter().map(|h| h.alias).collect();
        assert_eq!(aliases, ["gpu-lab", "gpu-box-2", "gpu2", "bastion"]);
    }

    #[test]
    fn a_host_reads_its_own_block() {
        assert_eq!(
            parse_text(FIXTURE)[0],
            host("gpu-lab", Some("10.0.4.21"), Some("lyle"), Some(22))
        );
    }

    #[test]
    fn keywords_are_case_insensitive_and_take_equals() {
        assert_eq!(
            parse_text(FIXTURE)[1],
            host("gpu-box-2", Some("gpu2.internal"), Some("lyle"), Some(2200))
        );
    }

    #[test]
    fn the_first_matching_value_wins_and_negation_excludes() {
        // `!bastion` keeps the `ops` block from applying to bastion.
        assert_eq!(
            parse_text(FIXTURE)[3],
            host("bastion", None, Some("fallback"), Some(22))
        );
    }

    #[test]
    fn match_blocks_are_not_applied() {
        let hosts = parse_text("Match all\n  User nobody\nHost a\n");
        assert_eq!(hosts, [host("a", None, None, None)]);
    }

    #[test]
    fn settings_before_any_host_apply_to_all() {
        let hosts = parse_text("User early\nHost a\n  User late\n");
        assert_eq!(hosts[0].user.as_deref(), Some("early"));
    }

    #[test]
    fn hostname_tokens_expand_the_alias() {
        let hosts = parse_text("Host box\n  HostName %h.example.com\n");
        assert_eq!(hosts[0].hostname.as_deref(), Some("box.example.com"));
    }

    #[test]
    fn quoted_arguments_stay_whole() {
        assert_eq!(arguments(r#""a b" c"#), ["a b", "c"]);
    }

    #[test]
    fn destination_shows_user_host_and_a_non_default_port() {
        let gpu = host("gpu", Some("10.0.0.1"), Some("lyle"), Some(2222));
        assert_eq!(gpu.destination(), "lyle@10.0.0.1:2222");
    }

    #[test]
    fn destination_falls_back_to_the_alias() {
        assert_eq!(host("box", None, None, Some(22)).destination(), "box");
    }

    #[test]
    fn wildcards_match_like_ssh() {
        assert!(wildcard_match("*.internal", "gpu2.internal"));
        assert!(wildcard_match("gpu?", "gpu2"));
        assert!(!wildcard_match("gpu?", "gpu22"));
    }

    #[test]
    fn includes_resolve_relative_to_the_config_directory() {
        let dir = scratch("include");
        std::fs::write(
            dir.join("config"),
            "Include config.d/*.conf extra\nHost main\n",
        )
        .unwrap();
        std::fs::write(dir.join("config.d/b.conf"), "Host b\n  User bee\n").unwrap();
        std::fs::write(dir.join("config.d/a.conf"), "Host a\n").unwrap();
        std::fs::write(dir.join("config.d/skip.txt"), "Host skipped\n").unwrap();
        std::fs::write(dir.join("extra"), "Host extra\n").unwrap();
        let aliases: Vec<String> = hosts(&dir.join("config"))
            .into_iter()
            .map(|h| h.alias)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(aliases, ["a", "b", "extra", "main"]);
    }

    #[test]
    fn a_missing_config_has_no_hosts() {
        assert!(hosts(Path::new("/nonexistent/ssh/config")).is_empty());
    }

    #[test]
    fn include_loops_stop() {
        let dir = scratch("loop");
        std::fs::write(dir.join("config"), "Host top\nInclude config\n").unwrap();
        let count = hosts(&dir.join("config")).len();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(count, 1);
    }
}
