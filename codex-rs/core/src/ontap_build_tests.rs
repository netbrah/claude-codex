use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_subcomponent_mapping() {
    assert_eq!(subcomponent_for_extension("cc"), Some("ulibso-l"));
    assert_eq!(subcomponent_for_extension("cpp"), Some("ulibso-l"));
    assert_eq!(subcomponent_for_extension("h"), Some("ulibso-l"));
    assert_eq!(subcomponent_for_extension("ut"), Some("utest-l"));
    assert_eq!(subcomponent_for_extension("rs"), None);
    assert_eq!(subcomponent_for_extension("py"), None);
    assert_eq!(subcomponent_for_extension("md"), None);
    assert_eq!(subcomponent_for_extension(""), None);
}

#[test]
fn test_find_component_dir_found() {
    let tmp = TempDir::new().unwrap();
    let comp_dir = tmp.path().join("security/keymanager");
    std::fs::create_dir_all(&comp_dir).unwrap();
    std::fs::write(comp_dir.join("Component.py"), "# marker").unwrap();

    let sub_dir = comp_dir.join("src/lib");
    std::fs::create_dir_all(&sub_dir).unwrap();

    let result = find_component_dir(&sub_dir);
    assert_eq!(result, Some(comp_dir));
}

#[test]
fn test_find_component_dir_at_start() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("Component.py"), "# marker").unwrap();

    let result = find_component_dir(tmp.path());
    assert_eq!(result, Some(tmp.path().to_path_buf()));
}

#[test]
fn test_find_component_dir_not_found() {
    let tmp = TempDir::new().unwrap();
    let deep = tmp.path().join("a/b/c/d");
    std::fs::create_dir_all(&deep).unwrap();

    let result = find_component_dir(&deep);
    assert!(result.is_none());
}

#[test]
fn test_plan_builds_groups_by_component() {
    let tmp = TempDir::new().unwrap();

    // Component A
    let comp_a = tmp.path().join("compA");
    std::fs::create_dir_all(comp_a.join("src")).unwrap();
    std::fs::write(comp_a.join("Component.py"), "").unwrap();
    let file_a1 = comp_a.join("src/foo.cc");
    let file_a2 = comp_a.join("src/bar.ut");
    std::fs::write(&file_a1, "").unwrap();
    std::fs::write(&file_a2, "").unwrap();

    // Component B
    let comp_b = tmp.path().join("compB");
    std::fs::create_dir_all(comp_b.join("src")).unwrap();
    std::fs::write(comp_b.join("Component.py"), "").unwrap();
    let file_b1 = comp_b.join("src/baz.cpp");
    std::fs::write(&file_b1, "").unwrap();

    let plan = plan_builds(&[file_a1, file_a2, file_b1]);

    assert_eq!(plan.len(), 2);
    assert_eq!(plan[&comp_a].len(), 2);
    assert_eq!(plan[&comp_b].len(), 1);

    // Check subcomponents
    let a_subs: Vec<&str> = plan[&comp_a].iter().map(|u| u.subcomponent).collect();
    assert!(a_subs.contains(&"ulibso-l"));
    assert!(a_subs.contains(&"utest-l"));

    assert_eq!(plan[&comp_b][0].subcomponent, "ulibso-l");
    assert_eq!(plan[&comp_b][0].stem, "baz");
}

#[test]
fn test_plan_builds_skips_non_buildable() {
    let tmp = TempDir::new().unwrap();
    let comp = tmp.path().join("comp");
    std::fs::create_dir_all(&comp).unwrap();
    std::fs::write(comp.join("Component.py"), "").unwrap();

    let md_file = comp.join("README.md");
    let py_file = comp.join("test.py");
    let rs_file = comp.join("main.rs");
    std::fs::write(&md_file, "").unwrap();
    std::fs::write(&py_file, "").unwrap();
    std::fs::write(&rs_file, "").unwrap();

    let plan = plan_builds(&[md_file, py_file, rs_file]);
    assert!(plan.is_empty());
}

#[test]
fn test_plan_builds_skips_files_outside_component() {
    let tmp = TempDir::new().unwrap();
    // No Component.py anywhere
    let dir = tmp.path().join("random/dir");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("foo.cc");
    std::fs::write(&file, "").unwrap();

    let plan = plan_builds(&[file]);
    assert!(plan.is_empty());
}

#[tokio::test]
async fn test_build_modified_files_returns_none_for_non_ontap() {
    let result = build_modified_files(&[
        PathBuf::from("/tmp/foo.md"),
        PathBuf::from("/tmp/bar.py"),
        PathBuf::from("/tmp/baz.rs"),
    ])
    .await;
    assert!(result.is_none());
}
