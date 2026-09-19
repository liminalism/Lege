//! User-facing path intake: drag-and-drop, `file://` URLs, and text lists.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

pub fn is_list_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("txt") || extension.eq_ignore_ascii_case("list")
    })
}

pub fn resolve_user_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    resolve_user_path_str(&raw)
}

pub fn resolve_user_path_str(raw: &str) -> PathBuf {
    let trimmed = raw
        .trim()
        .trim_matches(|character| character == '"' || character == '\'');
    if trimmed.is_empty() {
        return PathBuf::new();
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        return PathBuf::from(percent_decode(&strip_file_url_host(rest)));
    }
    PathBuf::from(percent_decode(trimmed))
}

fn strip_file_url_host(rest: &str) -> String {
    if rest.starts_with('/') {
        return rest.to_string();
    }
    if let Some(slash) = rest.find('/') {
        let host = &rest[..slash];
        if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            return rest[slash..].to_string();
        }
    }
    format!("/{rest}")
}

pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Some(value) = hex_byte(bytes[index + 1], bytes[index + 2])
        {
            output.push(value);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_byte(high: u8, low: u8) -> Option<u8> {
    Some(hex_digit(high)? << 4 | hex_digit(low)?)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// Split a drag-and-drop or pasted line into paths. Handles quotes, shell
/// escapes, `file://` URLs, and space-separated groups.
pub fn parse_path_input(input: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = ' ';
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        #[cfg(not(windows))]
        if character == '\\' {
            match chars.next() {
                Some(next @ (' ' | '\t' | '\'' | '"' | '\\')) => current.push(next),
                Some(next) => {
                    current.push('/');
                    current.push(next);
                }
                None => current.push('/'),
            }
            continue;
        }

        match character {
            '"' | '\'' if !in_quotes => {
                in_quotes = true;
                quote_char = character;
            }
            closer if in_quotes && closer == quote_char => {
                in_quotes = false;
                push_current(&mut paths, &mut current);
            }
            ' ' | '\t' if !in_quotes => push_current(&mut paths, &mut current),
            other => current.push(other),
        }
    }
    push_current(&mut paths, &mut current);
    if paths.is_empty() && !input.trim().is_empty() {
        paths.push(resolve_user_path_str(input));
    }
    paths
}

fn push_current(paths: &mut Vec<PathBuf>, current: &mut String) {
    if current.is_empty() {
        return;
    }
    paths.push(resolve_user_path_str(current));
    current.clear();
}

pub fn parse_list_file(path: &Path) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read list file {}: {error}", path.display()))?;
    let mut paths = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parsed = parse_path_input(trimmed);
        if parsed.is_empty() {
            return Err(format!(
                "{}:{}: could not parse path",
                path.display(),
                index + 1
            ));
        }
        paths.extend(parsed);
    }
    Ok(paths)
}

pub fn expand_inputs(inputs: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>, String> {
    let mut found = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for input in inputs {
        expand_one(input, recursive, None, &mut visited, &mut found)?;
    }
    Ok(found.into_iter().collect())
}

pub fn expand_one(
    path: &Path,
    recursive: bool,
    excluded_root: Option<&Path>,
    visited: &mut BTreeSet<PathBuf>,
    found: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let resolved = resolve_user_path(path);
    if resolved.as_os_str().is_empty() {
        return Ok(());
    }
    if resolved.is_file() {
        if is_pdf(&resolved) {
            if resolved
                .metadata()
                .map(|meta| meta.len() == 0)
                .unwrap_or(false)
            {
                eprintln!("lege-ocr: skipping empty PDF {}", resolved.display());
                return Ok(());
            }
            found.insert(resolved);
            return Ok(());
        }
        if is_list_file(&resolved) {
            let canonical = resolved
                .canonicalize()
                .map_err(|error| format!("{}: {error}", resolved.display()))?;
            if !visited.insert(canonical) {
                return Ok(());
            }
            for nested in parse_list_file(&resolved)? {
                expand_one(&nested, recursive, excluded_root, visited, found)?;
            }
            return Ok(());
        }
        return Err(format!(
            "input is not a PDF or path list: {}",
            resolved.display()
        ));
    }
    if !resolved.is_dir() {
        return Err(format!("input does not exist: {}", resolved.display()));
    }
    let canonical = resolved.canonicalize().map_err(|error| error.to_string())?;
    if excluded_root.is_some_and(|excluded| canonical.starts_with(excluded))
        || !visited.insert(canonical)
    {
        return Ok(());
    }
    for entry in std::fs::read_dir(&resolved).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let child = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() && recursive {
            expand_one(&child, true, excluded_root, visited, found)?;
        } else if child.is_file() && is_pdf(&child) {
            if child
                .metadata()
                .map(|meta| meta.len() == 0)
                .unwrap_or(false)
            {
                eprintln!("lege-ocr: skipping empty PDF {}", child.display());
                continue;
            }
            found.insert(child);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_urls_and_percent_encoding() {
        let path = resolve_user_path_str(
            "file:///home/dk/Downloads/%5BCambridge%5D%20Spencer%20-%20book.pdf",
        );
        assert_eq!(
            path,
            PathBuf::from("/home/dk/Downloads/[Cambridge] Spencer - book.pdf")
        );
    }

    #[test]
    fn parses_quoted_and_escaped_drag_drop_paths() {
        let paths = parse_path_input(r#""/tmp/A Book.pdf" /tmp/second.pdf"#);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/tmp/A Book.pdf"),
                PathBuf::from("/tmp/second.pdf")
            ]
        );
    }

    #[test]
    fn list_file_expands_urls_and_skips_comments() {
        let directory = tempfile::tempdir().unwrap();
        let pdf = directory.path().join("book.pdf");
        std::fs::write(&pdf, b"%PDF").unwrap();
        let list = directory.path().join("list.txt");
        std::fs::write(
            &list,
            format!(
                "# comment\nfile://{}\n",
                pdf.display().to_string().replace(' ', "%20")
            ),
        )
        .unwrap();
        let found = expand_inputs(&[list], false).unwrap();
        assert_eq!(found, vec![pdf]);
    }
}
