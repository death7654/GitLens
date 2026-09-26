use super::types::SubsystemDef;
use git2::{ObjectType, Repository};

/// Top-level directory names that are virtually never a meaningful
/// "subsystem" on their own — dependency caches, build output, VCS/editor
/// metadata. Filtered out so discovery doesn't hand back noise the caller
/// would just delete anyway.
const IGNORED_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    "vendor",
    "__pycache__",
    "venv",
];

/// Auto-discovers candidate subsystems from a repo's actual directory
/// layout, instead of requiring the caller to hand-type `path_prefixes`
/// blind. Looks only at HEAD's tree (a single `git show`-equivalent read,
/// no history walk), so it's cheap enough to call before every extraction
/// run or whenever the repo path field changes in Person 5's UI.
///
/// Each non-ignored top-level directory becomes one `SubsystemDef` named
/// after itself, with a single path prefix `"<dir>/"`. Root-level files
/// (README, Cargo.toml, etc.) are collected into one `"root"` subsystem
/// keyed by their exact paths, so they aren't silently dropped from the
/// mining window. Dotdirs (`.git`, `.github`, `.vscode`, ...) are always
/// skipped — the leading-dot check subsumes explicit VCS/editor-metadata
/// entries in `IGNORED_DIR_NAMES`, so that list only needs to cover
/// non-dot conventional names.
///
/// This is intentionally shallow (depth 1): a monorepo with `apps/web/`,
/// `apps/api/` nested a level deeper will surface as a single `apps`
/// subsystem here. That's a reasonable default for the common case (one
/// directory per subsystem at repo root) and the caller can always edit
/// the returned prefixes by hand for deeper layouts — discovery is meant
/// to save the common case from being hand-typed, not to replace editing
/// entirely.
pub fn discover_subsystems(repo_path: &str) -> Result<Vec<SubsystemDef>, String> {
    let repo = Repository::open(repo_path).map_err(|e| format!("open repo: {e}"))?;
    let head = repo.head().map_err(|e| format!("resolve HEAD: {e}"))?;
    let commit = head
        .peel_to_commit()
        .map_err(|e| format!("peel HEAD to commit: {e}"))?;
    let tree = commit.tree().map_err(|e| format!("read HEAD tree: {e}"))?;

    let mut subsystems = Vec::new();
    let mut root_files = Vec::new();

    for entry in tree.iter() {
        // Non-UTF8 names can't round-trip through the `String`-based
        // `SubsystemDef`/path-matching types elsewhere in this module, so
        // skip rather than lossily guess at a prefix that wouldn't
        // actually match anything during mining.
        let name = match entry.name() {
            Some(n) => n.to_string(),
            None => continue,
        };

        match entry.kind() {
            Some(ObjectType::Tree) => {
                if name.starts_with('.') || IGNORED_DIR_NAMES.contains(&name.as_str()) {
                    continue;
                }
                subsystems.push(SubsystemDef {
                    name: name.clone(),
                    path_prefixes: vec![format!("{}/", name)],
                });
            }
            Some(ObjectType::Blob) => {
                if !name.starts_with('.') {
                    root_files.push(name);
                }
            }
            // Submodules (Commit) and symlinks with no further structure to
            // walk here; leave them out rather than guess at a prefix.
            _ => {}
        }
    }

    if !root_files.is_empty() {
        root_files.sort();
        subsystems.push(SubsystemDef {
            name: "root".to_string(),
            path_prefixes: root_files,
        });
    }

    subsystems.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(subsystems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(path: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("git command failed to run");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn write(path: &std::path::Path, rel: &str, content: &str) {
        let p = path.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn discovers_top_level_dirs_and_groups_root_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        run(path, &["init", "-q", "-b", "main"]);

        write(path, "frontend/app.js", "console.log('hi');\n");
        write(path, "backend/server.rs", "fn main() {}\n");
        write(path, "node_modules/left-pad/index.js", "// dep\n");
        write(path, "README.md", "# hi\n");
        write(path, "Cargo.toml", "[package]\n");

        run(path, &["add", "."]);
        run(path, &["commit", "-q", "-m", "Initial commit"]);

        let subsystems = discover_subsystems(path.to_str().unwrap()).expect("discovery should succeed");
        let names: Vec<&str> = subsystems.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"frontend"));
        assert!(names.contains(&"backend"));
        assert!(!names.contains(&"node_modules"), "ignored dirs must not be surfaced");

        let frontend = subsystems.iter().find(|s| s.name == "frontend").unwrap();
        assert_eq!(frontend.path_prefixes, vec!["frontend/".to_string()]);

        let root = subsystems
            .iter()
            .find(|s| s.name == "root")
            .expect("root-level files should be grouped into a root subsystem");
        assert!(root.path_prefixes.contains(&"README.md".to_string()));
        assert!(root.path_prefixes.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn skips_dotdirs_and_empty_repo_yields_no_subsystems() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        run(path, &["init", "-q", "-b", "main"]);
        write(path, ".github/workflows/ci.yml", "name: ci\n");
        run(path, &["add", "."]);
        run(path, &["commit", "-q", "-m", "Add CI config"]);

        let subsystems = discover_subsystems(path.to_str().unwrap()).expect("discovery should succeed");
        assert!(
            subsystems.iter().all(|s| s.name != ".github"),
            "dotdirs must be skipped"
        );
    }
}
