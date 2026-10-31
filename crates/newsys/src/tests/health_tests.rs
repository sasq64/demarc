use std::path::Path;

use super::linux::*;
use super::*;

fn setup_script() -> String {
    let path = Path::new(env!("DEMARC_ROOT")).join("system/win/setup-wine.sh");
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn every_put_in_the_setup_script_is_checked() {
    for line in setup_script().lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("put") {
            continue;
        }
        let (file, dir) = (words.next().unwrap(), words.next().unwrap());
        assert!(
            PROXIES
                .iter()
                .chain(EXTRAS)
                .any(|item| *item == (file, dir)),
            "{file} in {dir} is not in PROXIES or EXTRAS"
        );
    }
}

#[test]
fn every_winetricks_verb_in_the_setup_script_is_checked() {
    for line in setup_script().lines() {
        let Some(verbs) = line.trim().strip_prefix("winetricks ") else {
            continue;
        };
        for verb in verbs.split_whitespace().filter(|v| !v.starts_with('-')) {
            assert!(
                verb == "corefonts" || NATIVE_DLLS.iter().any(|(name, _)| *name == verb),
                "winetricks {verb} is not in NATIVE_DLLS"
            );
        }
    }
}

#[test]
fn wine_versions() {
    assert_eq!(parse_wine_version("wine-11.17"), Some((11, 17)));
    assert_eq!(parse_wine_version("wine-11.17 (Staging)"), Some((11, 17)));
    assert_eq!(parse_wine_version("wine-10.0-rc1"), Some((10, 0)));
    assert_eq!(parse_wine_version("nonsense"), None);
}

#[test]
fn the_wanted_wine_version_comes_from_the_setup_script() {
    let wanted = crate::wine::wanted_version().unwrap();
    assert!(parse_wine_version(&wanted).is_some(), "{wanted}");
}

#[test]
fn hyprland_option() {
    assert_eq!(parse_hypr_option("int: 1\nset: true"), Some(1));
    assert_eq!(parse_hypr_option("no such option"), None);
}

#[test]
fn wine_stubs_do_not_count_as_native() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("stub.dll");
    let native = dir.path().join("native.dll");
    let mut bytes = vec![0; 0x40];
    bytes.extend_from_slice(b"Wine builtin DLL");
    std::fs::write(&stub, &bytes).unwrap();
    std::fs::write(&native, [0; 0x80]).unwrap();
    assert!(!is_native(&stub));
    assert!(is_native(&native));
    assert!(!is_native(&dir.path().join("absent.dll")));
}

#[test]
fn reg_values_are_found_under_their_own_key_only() {
    let hive =
        "[Software\\\\A] 123\n#time=1\n\"One\"=\"1\"\n\n[Software\\\\B] 456\n\"Two\"=\"2\"\n";
    assert!(has_reg_value(hive, r"Software\\A", r#""One"="1""#));
    assert!(has_reg_value(hive, r"software\\b", r#""two"="2""#));
    assert!(!has_reg_value(hive, r"Software\\A", r#""Two"="2""#));
    assert!(!has_reg_value(hive, r"Software\\C", r#""One"="1""#));
}

fn line<'a>(lines: &'a [Line], name: &str) -> &'a Line {
    lines.iter().find(|line| line.name == name).unwrap()
}

#[test]
fn a_prefix_that_is_not_there_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let lines = prefix_lines(&dir.path().join("none"), dir.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].status, Status::Missing);
}

#[test]
fn proxies_are_compared_with_the_bundled_copies() {
    let dir = tempfile::tempdir().unwrap();
    let (prefix, bundled) = (dir.path().join("prefix"), dir.path().join("bundled"));
    std::fs::create_dir(&bundled).unwrap();
    for dir in ["system32", "syswow64"] {
        std::fs::create_dir_all(prefix.join("drive_c/windows").join(dir)).unwrap();
    }
    for (file, dir) in PROXIES {
        std::fs::write(bundled.join(file), "new").unwrap();
        let path = prefix.join("drive_c/windows").join(dir).join(file);
        std::fs::write(path, "new").unwrap();
    }
    assert_eq!(
        line(&prefix_lines(&prefix, &bundled), "Proxy DLLs").status,
        Status::Ok
    );

    let win = prefix.join("drive_c/windows");
    std::fs::write(win.join("syswow64/winmm.dll"), "old").unwrap();
    let lines = prefix_lines(&prefix, &bundled);
    let proxies = line(&lines, "Proxy DLLs");
    assert_eq!(proxies.status, Status::Warn);
    assert!(
        proxies.detail.contains("outdated syswow64/winmm.dll"),
        "{}",
        proxies.detail
    );

    std::fs::remove_file(win.join("system32/dcomp.dll")).unwrap();
    let lines = prefix_lines(&prefix, &bundled);
    let proxies = line(&lines, "Proxy DLLs");
    assert_eq!(proxies.status, Status::Missing);
    assert!(
        proxies.detail.contains("missing system32/dcomp.dll"),
        "{}",
        proxies.detail
    );
}

#[test]
fn a_fix_is_listed_once_and_only_a_missing_line_fails_the_report() {
    let with = |status, fix: &str| Line {
        status,
        name: "x",
        detail: String::new(),
        fix: Some(fix.to_string()),
    };
    let report = Report {
        sections: vec![(
            "Section".to_string(),
            vec![with(Status::Warn, "Do it"), with(Status::Warn, "Do it")],
        )],
    };
    assert!(report.ok());
    assert_eq!(report.to_string().matches("Do it").count(), 1);

    let report = Report {
        sections: vec![("Section".to_string(), vec![with(Status::Missing, "Do it")])],
    };
    assert!(!report.ok());

    let report = Report {
        sections: vec![("Section".to_string(), vec![])],
    };
    assert!(report.to_string().ends_with("Everything is in order."));
}

#[test]
fn sizes_read_well() {
    assert_eq!(human_size(12), "12 bytes");
    assert_eq!(human_size(1536), "1.5 KB");
    assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
}
