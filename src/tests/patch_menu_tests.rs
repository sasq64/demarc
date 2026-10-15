use super::*;

const TEXT: &str = r#"[zoo.81]
meta = { dos4gw = "true" }

[zoo.829]
patch = {
  info = "x",
  target = "a.exe",
}
"#;

fn apply(id: u32, change: Change) -> String {
    let mut doc: DocumentMut = TEXT.parse().unwrap();
    edit(&mut doc, DemoId::new("zoo", id).unwrap(), &change).unwrap();
    let out = doc.to_string();
    overrides::parse(&out, Emulator::resolve_key_name).unwrap();
    out
}

#[test]
fn adds_to_existing_meta() {
    let out = apply(81, Change::Meta("wine_res", "640x480".into()));
    assert!(out.contains(r#"wine_res = "640x480""#), "{out}");
    assert!(out.contains("[zoo.829]\npatch = {\n"), "{out}");
}

#[test]
fn creates_entry() {
    let out = apply(5, Change::Boot("demo.exe".into()));
    assert!(out.contains("[zoo.5]\nboot = \"demo.exe\""), "{out}");
}

#[test]
fn appends_enters() {
    let out = apply(5, Change::Enters);
    let out = apply_to(&out, 5, Change::Enters);
    assert_eq!(out.matches(r#"key = "Enter""#).count(), 6, "{out}");
}

fn apply_to(text: &str, id: u32, change: Change) -> String {
    let mut doc: DocumentMut = text.parse().unwrap();
    edit(&mut doc, DemoId::new("zoo", id).unwrap(), &change).unwrap();
    doc.to_string()
}
