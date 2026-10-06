use super::*;

/// The parameter rows of a real preset, which is also where the cost of reading
/// them shows: a Mega Bezel preset is ~40 passes to preprocess.
///
/// `cargo test reads_preset_parameters -- --ignored --nocapture`.
#[test]
#[ignore = "needs the shaders/ working checkout"]
fn reads_preset_parameters() {
    let preset = Path::new(
        "shaders/shaders_slang/bezel/Mega_Bezel/Presets/Base_CRT_Presets/MBZ__0__SMOOTH-ADV__GDV.slangp",
    );
    let start = std::time::Instant::now();
    let params = preset_params(preset);
    println!("{} parameters in {:.2?}", params.len(), start.elapsed());
    assert!(!params.is_empty());
    for param in params.iter().take(5) {
        println!(
            "{} [{}..{} /{}]",
            param.name, param.min, param.max, param.step
        );
    }
    assert!(params.iter().all(|p| p.min <= p.max));
    // Every name is a row of its own.
    let mut names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count);
}
