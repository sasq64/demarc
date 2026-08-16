use std::collections::HashMap;

use crate::emu_file::EmuFile;

#[derive(Default, Debug, Clone)]
pub struct GameInfo {
    pub title: String,
    pub group: String,
    pub year: u32,
    pub category: String,
}

pub fn get_info_text(work_file: &EmuFile, meta: &HashMap<String, String>) -> String {
    let system = meta.get("system").cloned().unwrap_or("???".to_string()); //get_system_name(work_file);
    let GameInfo {
        title,
        group,
        year,
        category: typ,
    } = &work_file.game_info;
    let year = if *year == 0 {
        "".into()
    } else {
        format!(" ({year})")
    };
    let desc = if typ.is_empty() { &system } else { &typ };

    format!("\"{title}\"\n{group}\n{desc}{year}")
}
