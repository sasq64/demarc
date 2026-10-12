use super::*;

#[test]
fn failures_are_told_from_noise() {
    for line in [
        "wine: Call from 7B62E9C2 to unimplemented function d3dx9_36.dll.D3DXFoo, aborting",
        "0128:err:module:import_dll Library MSVCR90.dll (which is needed by L\"demo.exe\") not found",
        "Mesa: error: GL_INVALID_OPERATION in glUseProgram(program 1 not linked)",
        "0:1(2410): error: could not implicitly convert operands to arithmetic operator",
    ] {
        assert!(is_error(line), "{line}");
    }
    for line in [
        "0024:fixme:winediag:loader_init wine-staging is a testing version",
        "0128:fixme:d3dx:D3DXTessellateNPatches stub",
    ] {
        assert!(!is_error(line), "{line}");
    }
}

#[test]
fn a_line_is_logged_once() {
    let mut seen = HashSet::new();
    log_line("Mesa: error: x", &mut seen);
    log_line("Mesa: error: x", &mut seen);
    log_line("", &mut seen);
    assert_eq!(seen.len(), 1);
}
