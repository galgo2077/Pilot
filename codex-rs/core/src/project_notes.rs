//! Maintains the small, project-local Obsidian vault used by Codex edits.

use codex_git_utils::get_git_repo_root;
use sha1::{Digest, Sha1};
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

const VAULT_DIR: &str = "obsidian vault";
const COMPONENTS_DIR: &str = "Components";

/// Synchronize notes for source files changed by a successful native patch.
///
/// This deliberately runs only for local environments: remote filesystems already have their
/// own execution boundary and must not be mirrored to the host filesystem.
pub(crate) fn sync_project_notes(cwd: &Path, changed_paths: &[PathBuf]) -> io::Result<()> {
    let project_root = get_git_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let vault = project_root.join(VAULT_DIR);
    fs::create_dir_all(vault.join(COMPONENTS_DIR))?;
    write_if_missing(
        &vault.join("Architecture.md"),
        "# Architecture\n\nProject boundaries and contracts are documented by component notes.\n",
    )?;

    prune_missing_component_notes(&vault, &project_root)?;

    let mut component_links = Vec::new();
    for path in changed_paths {
        let Ok(relative) = path.strip_prefix(&project_root) else {
            continue;
        };
        if !is_meaningful_source(relative) {
            continue;
        }
        let note_name = note_name(relative);
        let note = vault.join(COMPONENTS_DIR).join(format!("{note_name}.md"));
        if path.exists() {
            let source = fs::read(path)?;
            if source.len() < 120 {
                if note.exists() {
                    fs::remove_file(&note)?;
                }
                continue;
            }
            let digest = Sha1::digest(&source);
            let lines = source.iter().filter(|byte| **byte == b'\n').count()
                + usize::from(!source.is_empty() && !source.ends_with(b"\n"));
            let contents = format!(
                "---\nsource: {}\nsource_hash: sha1:{digest:x}\nsource_bytes: {}\nsource_lines: {}\n---\n\n# {note_name}\n\n## Source\n\n`{}`\n\n## Purpose\n\nMaintained from successful Codex file changes; inspect the source for implementation details.\n\n## Changes\n\n- Source changed by a successful native patch. Tests and design decisions are recorded by the task that made the change.\n",
                relative.display(),
                source.len(),
                lines,
                relative.display()
            );
            if fs::read_to_string(&note).ok().as_deref() != Some(contents.as_str()) {
                fs::write(note, contents)?;
            }
            component_links.push(note_name);
        } else if note.exists() {
            fs::remove_file(note)?;
        }
    }
    update_home(&vault, &component_links)?;
    validate_wikilinks(&vault)
}

fn is_meaningful_source(path: &Path) -> bool {
    if path.starts_with(VAULT_DIR) || path.components().any(|part| part.as_os_str() == ".git") {
        return false;
    }
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("rs" | "py" | "js" | "ts" | "tsx" | "java" | "kt" | "go" | "cpp" | "c" | "h")
    )
}

fn note_name(path: &Path) -> String {
    path.to_string_lossy()
        .replace(['/', '\\', '.'], "_")
        .trim_end_matches('_')
        .to_owned()
}

fn write_if_missing(path: &Path, contents: &str) -> io::Result<()> {
    if !path.exists() {
        fs::write(path, contents)?;
    }
    Ok(())
}

fn update_home(vault: &Path, component_links: &[String]) -> io::Result<()> {
    let home = vault.join("Home.md");
    let mut contents = if home.exists() {
        fs::read_to_string(&home)?
    } else {
        "# Project knowledge\n\n[[Architecture]]\n\n## Components\n".to_owned()
    };
    contents = contents
        .lines()
        .filter(|line| {
            !line.starts_with("- [[Components/")
                || line
                    .strip_prefix("- [[Components/")
                    .and_then(|name| name.strip_suffix("]]"))
                    .is_some_and(|name| {
                        vault
                            .join(COMPONENTS_DIR)
                            .join(format!("{name}.md"))
                            .is_file()
                    })
        })
        .collect::<Vec<_>>()
        .join("\n");
    for link in component_links {
        let entry = format!("- [[Components/{link}]]");
        if !contents.contains(&entry) {
            contents.push_str(&format!("\n{entry}"));
        }
    }
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    fs::write(home, contents)
}

fn prune_missing_component_notes(vault: &Path, project_root: &Path) -> io::Result<()> {
    let components = vault.join(COMPONENTS_DIR);
    for entry in fs::read_dir(&components)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let Some(source) = text.lines().find_map(|line| line.strip_prefix("source: ")) else {
            continue;
        };
        let source = source.trim_matches('`');
        if !project_root.join(source).is_file() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn validate_wikilinks(vault: &Path) -> io::Result<()> {
    for entry in fs::read_dir(vault)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            validate_wikilinks(&path)?;
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        for link in text.match_indices("[[") {
            let remainder = &text[link.0 + 2..];
            let Some(end) = remainder.find("]]") else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unterminated wiki link",
                ));
            };
            let target = remainder[..end]
                .split(['|', '#'])
                .next()
                .unwrap_or_default();
            if target.is_empty() || !vault.join(format!("{target}.md")).is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("broken wiki link `{target}` in {}", path.display()),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn creates_a_vault_and_component_note_for_a_significant_source_file() -> io::Result<()> {
        let project = tempdir()?;
        fs::create_dir(project.path().join(".git"))?;
        let source = project.path().join("src").join("widget.rs");
        fs::create_dir_all(source.parent().expect("source parent"))?;
        fs::write(&source, "pub fn widget() {\n    let description = \"meaningful source\";\n    assert!(!description.is_empty());\n}\n".repeat(2))?;

        sync_project_notes(project.path(), std::slice::from_ref(&source))?;

        let vault = project.path().join(VAULT_DIR);
        assert!(vault.join("Home.md").is_file());
        assert!(vault.join("Architecture.md").is_file());
        assert!(
            vault
                .join(COMPONENTS_DIR)
                .join("src_widget_rs.md")
                .is_file()
        );
        validate_wikilinks(&vault)
    }

    #[test]
    fn leaves_tiny_source_files_without_component_notes() -> io::Result<()> {
        let project = tempdir()?;
        fs::create_dir(project.path().join(".git"))?;
        let source = project.path().join("tiny.rs");
        fs::write(&source, "const X: u8 = 1;\n")?;

        sync_project_notes(project.path(), std::slice::from_ref(&source))?;

        assert!(
            !project
                .path()
                .join(VAULT_DIR)
                .join(COMPONENTS_DIR)
                .join("tiny_rs.md")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn rebuilds_stale_notes_and_prunes_deleted_sources() -> io::Result<()> {
        let project = tempdir()?;
        fs::create_dir(project.path().join(".git"))?;
        let source = project.path().join("src").join("widget.rs");
        fs::create_dir_all(source.parent().expect("source parent"))?;
        fs::write(
            &source,
            "pub fn widget() {\n    let value = 1;\n}\n".repeat(5),
        )?;

        sync_project_notes(project.path(), std::slice::from_ref(&source))?;
        let note = project
            .path()
            .join(VAULT_DIR)
            .join(COMPONENTS_DIR)
            .join("src_widget_rs.md");
        let first = fs::read_to_string(&note)?;
        assert!(first.contains("source_hash: sha1:"));
        assert!(first.contains("source_bytes:"));
        assert!(first.contains("source_lines: 15"));

        fs::write(
            &source,
            "pub fn widget() {\n    let value = 2;\n}\n".repeat(5),
        )?;
        sync_project_notes(project.path(), std::slice::from_ref(&source))?;
        let second = fs::read_to_string(&note)?;
        assert_ne!(first, second);
        let first_hash = first.lines().find(|line| line.starts_with("source_hash: "));
        let second_hash = second
            .lines()
            .find(|line| line.starts_with("source_hash: "));
        assert_ne!(first_hash, second_hash);

        fs::remove_file(&source)?;
        sync_project_notes(project.path(), std::slice::from_ref(&source))?;
        assert!(!note.exists());
        assert!(
            !fs::read_to_string(project.path().join(VAULT_DIR).join("Home.md"))?
                .contains("[[Components/src_widget_rs]]")
        );
        Ok(())
    }
}
