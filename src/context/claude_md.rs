// Loading of project instruction files (AGENTS.md / CLAUDE.md / FINCH.md / CONTEXT.md /
// README.md) into the system prompt. See ASSEMBLY.md for the full contract.
//
// Precedence, lowest first (later sections win): `~/.claude/CLAUDE.md`, `~/.finch/FINCH.md`,
// then each directory from the filesystem root down to the working directory, and within one
// directory AGENTS.md → CLAUDE.md → FINCH.md → CONTEXT.md → README.md. AGENTS.md is the
// cross-tool convention, so Finch/Claude-specific files refine it; README.md is overview
// context and keeps its last position only for compatibility.
//
// A file reached more than once (a symlink, a hardlink, or a user-level file that points into
// the project) is included once, at its last and therefore highest-precedence position.
// Distinct files with identical text are both included. Files over
// `MAX_INSTRUCTION_FILE_BYTES` are skipped rather than truncated mid-rule. Every file found
// is reported in `InstructionSources` with what happened to it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Filenames we look for, in the order they are loaded within a single directory.
const CONTEXT_FILENAMES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "FINCH.md",
    "CONTEXT.md",
    "README.md",
];

/// Largest instruction file that is loaded; larger files are skipped and reported.
pub const MAX_INSTRUCTION_FILE_BYTES: u64 = 256 * 1024;

/// What happened to one instruction file that exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    /// Included in the system prompt.
    Loaded,
    /// Present but blank.
    Empty,
    /// The same file is reached again later, at a higher-precedence path.
    SupersededBy(PathBuf),
    /// Larger than [`MAX_INSTRUCTION_FILE_BYTES`].
    TooLarge { bytes: u64 },
    /// Not a regular file, or could not be read.
    Unreadable(String),
}

/// One instruction file found while collecting, in precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSource {
    pub path: PathBuf,
    pub status: SourceStatus,
}

/// The instruction files found for a working directory and the text assembled from them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionSources {
    /// Every existing candidate, lowest precedence first.
    pub sources: Vec<InstructionSource>,
    text: Option<String>,
}

impl InstructionSources {
    /// The assembled instructions, or `None` when nothing was loaded.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Paths whose contents were included, in prompt order.
    pub fn loaded(&self) -> impl Iterator<Item = &Path> {
        self.sources
            .iter()
            .filter(|source| source.status == SourceStatus::Loaded)
            .map(|source| source.path.as_path())
    }
}

/// Collect the instructions visible from `cwd` using the real home directory.
///
/// Returns `None` if no files were found or all were empty.
pub fn collect_claude_md_context(cwd: &Path) -> Option<String> {
    collect_instructions(cwd, dirs::home_dir().as_deref())
        .text()
        .map(str::to_owned)
}

/// Collect the instructions visible from `cwd`, reading user-level files under `home`.
pub fn collect_instructions(cwd: &Path, home: Option<&Path>) -> InstructionSources {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join(".claude").join("CLAUDE.md"));
        candidates.push(home.join(".finch").join("FINCH.md"));
    }
    let mut ancestors: Vec<&Path> = cwd.ancestors().collect();
    ancestors.reverse(); // root first, cwd last
    for dir in ancestors {
        for filename in CONTEXT_FILENAMES {
            candidates.push(dir.join(filename));
        }
    }

    let mut entries: Vec<(InstructionSource, Option<(FileIdentity, String)>)> = Vec::new();
    for path in candidates {
        let Some(entry) = inspect(path) else { continue };
        entries.push(entry);
    }

    // Keep each file only at its last (highest-precedence) position.
    let mut last_position: HashMap<FileIdentity, usize> = HashMap::new();
    for (index, (_, loaded)) in entries.iter().enumerate() {
        if let Some((identity, _)) = loaded {
            last_position.insert(identity.clone(), index);
        }
    }
    let paths: Vec<PathBuf> = entries
        .iter()
        .map(|(source, _)| source.path.clone())
        .collect();
    let mut sections: Vec<String> = Vec::new();
    let mut sources: Vec<InstructionSource> = Vec::with_capacity(entries.len());
    for (index, (mut source, loaded)) in entries.into_iter().enumerate() {
        if let Some((identity, content)) = loaded {
            let last = last_position[&identity];
            if last == index {
                sections.push(format!(
                    "From `{}`:\n\n{}",
                    source.path.display(),
                    content.trim_end()
                ));
            } else {
                source.status = SourceStatus::SupersededBy(paths[last].clone());
            }
        }
        debug!(path = %source.path.display(), status = ?source.status, "instruction source");
        sources.push(source);
    }

    let text = (!sections.is_empty()).then(|| sections.join("\n\n---\n\n"));
    if text.is_none() {
        debug!("No instruction files loaded from {}", cwd.display());
    }
    InstructionSources { sources, text }
}

/// Identity of the file a path resolves to, so every route to one file counts once.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FileIdentity {
    #[cfg(unix)]
    Inode { device: u64, inode: u64 },
    #[cfg(not(unix))]
    Canonical(PathBuf),
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata, _path: &Path) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity::Inode {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata, path: &Path) -> FileIdentity {
    FileIdentity::Canonical(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// Inspect one candidate; `None` when it does not exist. Loadable files carry their content.
fn inspect(path: PathBuf) -> Option<(InstructionSource, Option<(FileIdentity, String)>)> {
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A symlink whose target is gone still exists; report it rather than vanish.
            let dangling = fs::symlink_metadata(&path).is_ok();
            let status = SourceStatus::Unreadable("dangling symlink".into());
            return dangling.then(|| (InstructionSource { path, status }, None));
        }
        Err(error) => {
            let status = SourceStatus::Unreadable(error.to_string());
            return Some((InstructionSource { path, status }, None));
        }
    };
    let status = |status| InstructionSource {
        path: path.clone(),
        status,
    };
    if !metadata.is_file() {
        return Some((
            status(SourceStatus::Unreadable("not a regular file".into())),
            None,
        ));
    }
    if metadata.len() > MAX_INSTRUCTION_FILE_BYTES {
        let bytes = metadata.len();
        return Some((status(SourceStatus::TooLarge { bytes }), None));
    }
    // Read at most one byte past the cap so a file that grows after `metadata` stays bounded.
    let read = fs::File::open(&path).and_then(|file| {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_INSTRUCTION_FILE_BYTES + 1),
            &mut bytes,
        )?;
        Ok(bytes)
    });
    let read = match read {
        Ok(bytes) if bytes.len() as u64 > MAX_INSTRUCTION_FILE_BYTES => {
            let bytes = bytes.len() as u64;
            return Some((status(SourceStatus::TooLarge { bytes }), None));
        }
        Ok(bytes) => String::from_utf8(bytes).map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match read {
        Ok(content) if content.trim().is_empty() => Some((status(SourceStatus::Empty), None)),
        Ok(content) => {
            let identity = file_identity(&metadata, &path);
            Some((status(SourceStatus::Loaded), Some((identity, content))))
        }
        Err(error) => Some((status(SourceStatus::Unreadable(error)), None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A project tree and an isolated home, so the developer's own files never leak in.
    struct Tree {
        root: TempDir,
        home: TempDir,
    }

    impl Tree {
        fn new() -> Self {
            Self {
                root: TempDir::new().unwrap(),
                home: TempDir::new().unwrap(),
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.path().join(relative)
        }

        fn write(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }

        fn collect(&self, cwd: &str) -> InstructionSources {
            collect_instructions(&self.path(cwd), Some(self.home.path()))
        }

        /// Statuses for sources inside this tree or its home, ignoring anything above /tmp.
        fn statuses(&self, sources: &InstructionSources) -> Vec<(PathBuf, SourceStatus)> {
            sources
                .sources
                .iter()
                .filter(|s| {
                    s.path.starts_with(self.root.path()) || s.path.starts_with(self.home.path())
                })
                .map(|s| (s.path.clone(), s.status.clone()))
                .collect()
        }
    }

    fn text(sources: &InstructionSources) -> &str {
        sources.text().expect("expected instructions to be loaded")
    }

    fn position(text: &str, marker: &str) -> usize {
        text.find(marker).unwrap_or_else(|| {
            panic!("marker {marker:?} missing from assembled instructions:\n{text}")
        })
    }

    #[test]
    fn returns_no_tree_sources_when_no_context_files() {
        let tree = Tree::new();
        let sources = tree.collect("");
        assert!(
            tree.statuses(&sources).is_empty(),
            "an empty tree and home must contribute no sources: {:?}",
            sources.sources
        );
    }

    #[test]
    fn loads_agents_md_from_cwd() {
        let tree = Tree::new();
        let agents = tree.write("AGENTS.md", "shared agent rules");
        let sources = tree.collect("");
        assert!(
            text(&sources).contains("shared agent rules"),
            "AGENTS.md must be loaded"
        );
        assert_eq!(
            tree.statuses(&sources),
            vec![(agents, SourceStatus::Loaded)],
            "provenance must report AGENTS.md as loaded"
        );
    }

    #[test]
    fn loads_all_names_in_same_directory() {
        let tree = Tree::new();
        for (name, marker) in [
            ("AGENTS.md", "agents-marker"),
            ("CLAUDE.md", "claude-marker"),
            ("FINCH.md", "finch-marker"),
            ("CONTEXT.md", "context-marker"),
            ("README.md", "readme-marker"),
        ] {
            tree.write(name, marker);
        }
        let sources = tree.collect("");
        let text = text(&sources);
        let order: Vec<usize> = [
            "agents-marker",
            "claude-marker",
            "finch-marker",
            "context-marker",
            "readme-marker",
        ]
        .iter()
        .map(|marker| position(text, marker))
        .collect();
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "within one directory the order must be AGENTS → CLAUDE → FINCH → CONTEXT → README; positions={order:?}\n{text}"
        );
    }

    #[test]
    fn joins_multiple_sections_with_separator() {
        let tree = Tree::new();
        tree.write("CLAUDE.md", "outer instructions");
        tree.write("subdir/AGENTS.md", "inner instructions");
        let sources = tree.collect("subdir");
        let text = text(&sources);
        assert!(
            position(text, "outer instructions") < position(text, "inner instructions"),
            "the working directory must come after (and so win over) its ancestors:\n{text}"
        );
        assert!(
            text.contains("\n\n---\n\n"),
            "sections must be separated:\n{text}"
        );
    }

    #[test]
    fn each_section_names_its_source_path() {
        let tree = Tree::new();
        let agents = tree.write("AGENTS.md", "rules");
        let sources = tree.collect("");
        let expected = format!("From `{}`:\n\nrules", agents.display());
        assert!(
            text(&sources).contains(&expected),
            "section must be introduced by its path; expected {expected:?} in:\n{}",
            text(&sources)
        );
    }

    #[test]
    fn user_level_files_precede_project_files() {
        let tree = Tree::new();
        fs::create_dir_all(tree.home.path().join(".claude")).unwrap();
        fs::create_dir_all(tree.home.path().join(".finch")).unwrap();
        fs::write(tree.home.path().join(".claude/CLAUDE.md"), "user-claude").unwrap();
        fs::write(tree.home.path().join(".finch/FINCH.md"), "user-finch").unwrap();
        tree.write("AGENTS.md", "project-agents");
        let sources = tree.collect("");
        let text = text(&sources);
        assert!(
            position(text, "user-claude") < position(text, "user-finch")
                && position(text, "user-finch") < position(text, "project-agents"),
            "user-level files must precede project files:\n{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_agents_md_loads_once_at_the_later_position() {
        let tree = Tree::new();
        let claude = tree.write("CLAUDE.md", "shared-rules");
        let agents = tree.path("AGENTS.md");
        std::os::unix::fs::symlink("CLAUDE.md", &agents).unwrap();
        let sources = tree.collect("");
        let text = text(&sources);
        assert_eq!(
            text.matches("shared-rules").count(),
            1,
            "a symlinked AGENTS.md must not inject the same rules twice:\n{text}"
        );
        assert_eq!(
            tree.statuses(&sources),
            vec![
                (agents, SourceStatus::SupersededBy(claude.clone())),
                (claude, SourceStatus::Loaded),
            ],
            "provenance must show AGENTS.md superseded by the CLAUDE.md it points to"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_file_loads_once() {
        let tree = Tree::new();
        let agents = tree.write("AGENTS.md", "linked-rules");
        let finch = tree.path("FINCH.md");
        fs::hard_link(&agents, &finch).unwrap();
        let sources = tree.collect("");
        assert_eq!(
            text(&sources).matches("linked-rules").count(),
            1,
            "a hardlink is the same file and must load once: {:?}",
            sources.sources
        );
        assert_eq!(
            tree.statuses(&sources),
            vec![
                (agents, SourceStatus::SupersededBy(finch.clone())),
                (finch, SourceStatus::Loaded),
            ],
            "provenance must report the earlier hardlink superseded by the later one: {:?}",
            sources.sources
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_level_file_linked_into_the_project_appears_at_the_project_position() {
        let tree = Tree::new();
        let project = tree.write("CLAUDE.md", "one-copy");
        fs::create_dir_all(tree.home.path().join(".claude")).unwrap();
        fs::create_dir_all(tree.home.path().join(".finch")).unwrap();
        let user = tree.home.path().join(".claude/CLAUDE.md");
        std::os::unix::fs::symlink(&project, &user).unwrap();
        fs::write(tree.home.path().join(".finch/FINCH.md"), "user-finch").unwrap();
        let sources = tree.collect("");
        let text = text(&sources);
        assert_eq!(
            text.matches("one-copy").count(),
            1,
            "linked file must load once:\n{text}"
        );
        assert!(
            position(text, "user-finch") < position(text, "one-copy"),
            "the linked file must sit at its project position, after later user-level files:\n{text}"
        );
        assert_eq!(
            tree.statuses(&sources)[0],
            (user, SourceStatus::SupersededBy(project)),
            "the user-level route must be reported as superseded by the project path: {:?}",
            sources.sources
        );
    }

    #[test]
    fn distinct_files_with_identical_text_both_load() {
        let tree = Tree::new();
        tree.write("AGENTS.md", "repeated-rule");
        tree.write("CLAUDE.md", "repeated-rule");
        let sources = tree.collect("");
        assert_eq!(
            text(&sources).matches("repeated-rule").count(),
            2,
            "identical text in two distinct files is the author repeating themselves: {:?}",
            sources.sources
        );
    }

    #[test]
    fn oversized_file_is_skipped_and_reported() {
        let tree = Tree::new();
        let big = tree.write(
            "AGENTS.md",
            &"x".repeat(MAX_INSTRUCTION_FILE_BYTES as usize + 1),
        );
        tree.write("CLAUDE.md", "small-rules");
        let sources = tree.collect("");
        assert!(
            !text(&sources).contains("xxxx"),
            "oversized file must not be loaded"
        );
        assert_eq!(
            tree.statuses(&sources)[0],
            (
                big,
                SourceStatus::TooLarge {
                    bytes: MAX_INSTRUCTION_FILE_BYTES + 1
                }
            ),
            "an oversized file must be reported as too large, not loaded or truncated: {:?}",
            sources.sources
        );
    }

    #[test]
    fn empty_and_unreadable_candidates_are_reported_not_loaded() {
        let tree = Tree::new();
        let empty = tree.write("CLAUDE.md", "   \n   ");
        let directory = tree.path("AGENTS.md");
        fs::create_dir_all(&directory).unwrap();
        let sources = tree.collect("");
        assert!(
            tree.statuses(&sources)
                .iter()
                .all(|(_, status)| *status != SourceStatus::Loaded),
            "nothing in the tree is loadable, so nothing in it may be loaded: {:?}",
            sources.sources
        );
        assert_eq!(
            tree.statuses(&sources),
            vec![
                (
                    directory,
                    SourceStatus::Unreadable("not a regular file".into())
                ),
                (empty, SourceStatus::Empty),
            ],
            "empty and non-file candidates must be reported with their reason: {:?}",
            sources.sources
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_reported_not_skipped() {
        let tree = Tree::new();
        let agents = tree.path("AGENTS.md");
        std::os::unix::fs::symlink("CLAUDE.md", &agents).unwrap();
        let sources = tree.collect("");
        assert_eq!(
            tree.statuses(&sources),
            vec![(agents, SourceStatus::Unreadable("dangling symlink".into()))],
            "an AGENTS.md pointing at a missing CLAUDE.md must be visible in provenance: {:?}",
            sources.sources
        );
    }
}
