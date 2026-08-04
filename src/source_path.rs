use std::io;
use std::path::{Component, MAIN_SEPARATOR, Path, PathBuf};

/// Resolves `path` without consulting the filesystem.
pub fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                // A drive-relative Windows prefix (`C:`) is not a root and cannot absorb `..`.
                Some(Component::RootDir) => {}
                _ => normalized.push(component.as_os_str()),
            },
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Returns the protocol identity for an existing local source path.
///
/// Filesystem canonicalization supplies one on-disk spelling for ordinary path
/// aliases while preserving names that are distinct on case-sensitive storage.
pub fn canonical_identity(
    canonical_workspace_root: &Path,
    lexical_path: &Path,
) -> io::Result<String> {
    let canonical_path = lexical_path.canonicalize()?;
    Ok(identity_from_canonical_paths(
        canonical_workspace_root,
        lexical_path,
        &canonical_path,
    ))
}

/// Makes workspace sources relative without canonicalizing outside-workspace
/// paths into a machine-specific spelling.
fn identity_from_canonical_paths(
    canonical_workspace_root: &Path,
    lexical_path: &Path,
    canonical_path: &Path,
) -> String {
    let identity = canonical_path
        .strip_prefix(canonical_workspace_root)
        .unwrap_or(lexical_path);
    protocol_path(identity)
}

/// Returns a case-preserving identity for a remapped or synthetic source.
///
/// These sources cannot be resolved through the local filesystem, so only
/// lexical normalization and exact workspace-prefix stripping are sound.
pub fn lexical_identity(workspace_root: &Path, path: &Path) -> String {
    let path = lexically_normalize(path);
    let identity = path.strip_prefix(workspace_root).unwrap_or(&path);
    protocol_path(identity)
}

fn protocol_path(path: &Path) -> String {
    path.to_string_lossy().replace(MAIN_SEPARATOR, "/")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{identity_from_canonical_paths, lexical_identity, lexically_normalize};

    #[test]
    fn lexical_paths_resolve_without_escaping_the_root() {
        assert_eq!(
            lexically_normalize(Path::new("/workspace/library/../src/./lib.rs")),
            Path::new("/workspace/src/lib.rs")
        );
        assert_eq!(
            lexically_normalize(Path::new("/../workspace/src/lib.rs")),
            Path::new("/workspace/src/lib.rs")
        );
        assert_eq!(
            lexically_normalize(Path::new("../../workspace/src/lib.rs")),
            Path::new("../../workspace/src/lib.rs")
        );
    }

    #[cfg(windows)]
    #[test]
    fn drive_relative_paths_preserve_parents_beyond_the_prefix() {
        assert_eq!(
            lexically_normalize(Path::new(r"C:..\workspace\src\lib.rs")),
            Path::new(r"C:..\workspace\src\lib.rs")
        );
        assert_eq!(
            lexically_normalize(Path::new(r"C:workspace\..\..\src\lib.rs")),
            Path::new(r"C:..\src\lib.rs")
        );
    }

    #[test]
    fn canonical_spelling_unifies_aliases() {
        let workspace_root = Path::new("/Workspace");
        let canonical = Path::new("/Workspace/Library/src/Shared.rs");

        assert_eq!(
            identity_from_canonical_paths(
                workspace_root,
                Path::new("/workspace/library/src/shared.rs"),
                canonical,
            ),
            "Library/src/Shared.rs"
        );
        assert_eq!(
            identity_from_canonical_paths(workspace_root, canonical, canonical),
            "Library/src/Shared.rs"
        );
    }

    #[test]
    fn canonical_spelling_preserves_case_distinct_files() {
        let workspace_root = Path::new("/workspace");

        assert_ne!(
            identity_from_canonical_paths(
                workspace_root,
                Path::new("/workspace/src/Foo.rs"),
                Path::new("/workspace/src/Foo.rs"),
            ),
            identity_from_canonical_paths(
                workspace_root,
                Path::new("/workspace/src/foo.rs"),
                Path::new("/workspace/src/foo.rs"),
            )
        );
    }

    #[test]
    fn outside_workspace_paths_keep_their_lexical_spelling() {
        assert_eq!(
            identity_from_canonical_paths(
                Path::new("/workspace"),
                Path::new("/alias/generated.rs"),
                Path::new("/generated/generated.rs"),
            ),
            "/alias/generated.rs"
        );
    }

    #[test]
    fn non_local_paths_are_workspace_relative_when_possible() {
        assert_eq!(
            lexical_identity(
                Path::new("/workspace"),
                Path::new("/workspace/library/../src/lib.rs"),
            ),
            "src/lib.rs"
        );
    }
}
