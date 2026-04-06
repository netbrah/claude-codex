use codex_file_search::FileSearchOptions;
use codex_file_search::FileSearchResults;
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[tokio::test]
async fn finds_files_by_fuzzy_pattern() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    std::fs::write(dir_path.join("hello_world.rs"), b"fn main() {}").expect("write file");
    std::fs::write(dir_path.join("utils.rs"), b"pub fn util() {}").expect("write file");
    std::fs::write(dir_path.join("readme.md"), b"# Readme").expect("write file");

    #[expect(clippy::unwrap_used)]
    let result = codex_file_search::run(
        "hello",
        vec![dir_path.to_path_buf()],
        FileSearchOptions {
            limit: NonZero::new(50).unwrap(),
            exclude: Vec::new(),
            threads: NonZero::new(2).unwrap(),
            compute_indices: false,
            respect_gitignore: false,
        },
        Some(Arc::new(AtomicBool::new(false))),
    )
    .expect("run file search");

    assert!(
        !result.matches.is_empty(),
        "should find at least one match for 'hello'"
    );
    let paths: Vec<String> = result
        .matches
        .iter()
        .map(|m| m.path.to_string_lossy().to_string())
        .collect();
    assert!(
        paths.iter().any(|p| p.contains("hello_world")),
        "should match hello_world.rs, got: {paths:?}"
    );
}

#[tokio::test]
async fn returns_empty_for_no_match() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    std::fs::write(dir_path.join("alpha.txt"), b"content").expect("write file");
    std::fs::write(dir_path.join("beta.txt"), b"content").expect("write file");

    #[expect(clippy::unwrap_used)]
    let result = codex_file_search::run(
        "zzzznonexistent",
        vec![dir_path.to_path_buf()],
        FileSearchOptions {
            limit: NonZero::new(50).unwrap(),
            exclude: Vec::new(),
            threads: NonZero::new(2).unwrap(),
            compute_indices: false,
            respect_gitignore: false,
        },
        Some(Arc::new(AtomicBool::new(false))),
    )
    .expect("run file search");

    assert!(result.matches.is_empty(), "should find no matches");
}

#[tokio::test]
async fn finds_nested_files() {
    let temp = tempdir().expect("create tempdir");
    let dir_path = temp.path();
    let sub = dir_path.join("src").join("components");
    std::fs::create_dir_all(&sub).expect("create subdirs");
    std::fs::write(sub.join("button.tsx"), b"export const Button = () => {};")
        .expect("write file");
    std::fs::write(dir_path.join("index.ts"), b"export {};").expect("write file");

    #[expect(clippy::unwrap_used)]
    let result = codex_file_search::run(
        "button",
        vec![dir_path.to_path_buf()],
        FileSearchOptions {
            limit: NonZero::new(50).unwrap(),
            exclude: Vec::new(),
            threads: NonZero::new(2).unwrap(),
            compute_indices: false,
            respect_gitignore: false,
        },
        Some(Arc::new(AtomicBool::new(false))),
    )
    .expect("run file search");

    assert!(
        !result.matches.is_empty(),
        "should find button.tsx in nested directory"
    );
    let paths: Vec<String> = result
        .matches
        .iter()
        .map(|m| m.path.to_string_lossy().to_string())
        .collect();
    assert!(
        paths.iter().any(|p| p.contains("button")),
        "should match button.tsx, got: {paths:?}"
    );
}
