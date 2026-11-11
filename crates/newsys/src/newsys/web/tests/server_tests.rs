use super::*;

#[test]
fn resolves_under_its_root_only() {
    let roots = vec![PathBuf::from("/r/a"), PathBuf::from("/r/b")];
    assert_eq!(
        resolve(&roots, "/1/js/main.js?v=2"),
        Some(PathBuf::from("/r/b/js/main.js"))
    );
    assert_eq!(
        resolve(&roots, "/0/my%20page.html"),
        Some(PathBuf::from("/r/a/my page.html"))
    );
    assert_eq!(resolve(&roots, "/0/../b/x.js"), None);
    assert_eq!(resolve(&roots, "/0/%2e%2e/b/x.js"), None);
    assert_eq!(resolve(&roots, "/2/x.js"), None);
    assert_eq!(resolve(&roots, "/x/x.js"), None);
}

#[test]
fn serves_wasm_as_wasm() {
    assert_eq!(content_type(Path::new("a.WASM")), "application/wasm");
    assert_eq!(content_type(Path::new("a.html")), "text/html");
}

#[test]
fn serves_a_page() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<p>hi</p>").unwrap();
    let url = url_for(&dir.path().join("index.html")).unwrap();
    let body = ureq::get(&url)
        .call()
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
    assert_eq!(body, "<p>hi</p>");
}
