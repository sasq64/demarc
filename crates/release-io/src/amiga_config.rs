use crate::workfile::WorkFile;
use anyhow::Result;
use tracing::info;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kickstart {
    V12,
    V13,
    V20,
    V31A1200,
    V31A4000,
}

impl From<Kickstart> for &'static str {
    fn from(value: Kickstart) -> Self {
        match value {
            Kickstart::V12 => "kick33180.A500",
            Kickstart::V13 => "kick34005.A500",
            Kickstart::V20 => "kick37175.A500",
            Kickstart::V31A1200 => "kick40068.A1200",
            Kickstart::V31A4000 => "kick40068.A4000",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Machine {
    A500_KS12,
    A500_512KB,
    A500,
    A500PLUS,
    A1200,
    A1230,
    A1200FAST,
    A1240,
    A4000,
}

impl std::str::FromStr for Machine {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_uppercase().as_str() {
            "A500_KS12" => Machine::A500_KS12,
            "A500_512KB" => Machine::A500_512KB,
            "A500" => Machine::A500,
            "A500PLUS" | "A500P" => Machine::A500PLUS,
            "A1200" => Machine::A1200,
            "A1230" => Machine::A1230,
            "A1200FAST" => Machine::A1200FAST,
            "A1240" => Machine::A1240,
            "A4000" => Machine::A4000,
            _ => anyhow::bail!("Unknown Amiga model '{s}'"),
        })
    }
}

// impl From<Machine> for String {
//     fn from(value: Machine) -> Self {
//         (match value {
//             Machine::A500_KS12 => "A500OG",
//             Machine::A500_512KB => "A500",
//             Machine::A500 => "A500",
//             Machine::A500PLUS => "A500P",
//             Machine::A1230 => "A1230",
//             Machine::A1200 => "A1200OG",
//             Machine::A1200FAST => "A1200",
//             Machine::A4000 => "A4000",
//         })
//         .into()
//     }
// }

pub trait AmigaMeta {
    fn set_machine(&mut self, machine: Machine);
    fn is_aga(&self) -> bool;
}

impl AmigaMeta for WorkFile {
    fn is_aga(&self) -> bool {
        !self.get_meta_or("amiberry_model", "").contains("500")
    }
    fn set_machine(&mut self, machine: Machine) {
        let settings = match machine {
            Machine::A500_KS12 => {
                vec![
                    ("amiberry_model", "A500"),
                    ("amiberry_kickstart", Kickstart::V12.into()),
                ]
            }
            Machine::A500_512KB => {
                vec![
                    ("amiberry_model", "A500"),
                    ("amiberry_slowmem_size", "0"),
                    ("amiberry_jit", "disabled"),
                ]
            }
            Machine::A500 => {
                vec![("amiberry_model", "A500"), ("amiberry_jit", "disabled")]
            }
            Machine::A500PLUS => {
                vec![
                    ("amiberry_model", "A500P"),
                    ("amiberry_chipmem_size", "4"),
                    ("amiberry_jit", "disabled"),
                ]
            }
            Machine::A1200 => {
                vec![("amiberry_model", "A1200OG"), ("amiberry_jit", "enabled")]
            }
            Machine::A1200FAST => {
                vec![
                    ("amiberry_model", "A1200"),
                    ("amiberry_fastmem_size", "8"),
                    ("amiberry_jit", "enabled"),
                ]
            }
            Machine::A1230 => {
                vec![
                    ("amiberry_model", "A1200"),
                    ("amiberry_fastmem_size", "8"),
                    ("amiberry_z3mem_size", "64"),
                    ("amiberry_cpu_model", "68030"),
                    ("amiberry_cpu_speed", "max"),
                    ("amiberry_fpu_model", "68882"),
                    ("amiberry_jit", "enabled"),
                ]
            }
            Machine::A1240 => {
                vec![
                    ("amiberry_model", "A1200"),
                    ("amiberry_fastmem_size", "8"),
                    ("amiberry_z3mem_size", "64"),
                    ("amiberry_cpu_model", "68040"),
                    ("amiberry_fpu_model", "68040"),
                    ("amiberry_cpu_speed", "max"),
                    ("amiberry_jit", "enabled"),
                ]
            }
            Machine::A4000 => {
                vec![
                    ("amiberry_model", "A4040"),
                    ("amiberry_fastmem_size", "8"),
                    ("amiberry_z3mem_size", "64"),
                    //("aiberry_cpu_model", "68040"),
                    //("aiberry_fpu_model", "68882"),
                    ("amiberry_cpu_speed", "max"),
                    ("amiberry_jit", "enabled"),
                ]
            }
        };
        for (key, val) in settings {
            if !self.has_meta(key) {
                self.set_meta(key, val);
            }
        }
    }
}

pub fn set_amiga_config(file: &mut WorkFile) -> Result<()> {
    let mut machine = Machine::A500;

    let aga =
        file.get_meta_or("platform", "").contains("AGA") || file.get_meta_or("aga", "") == "true";

    let jit = file.get_meta_or("jit", "");
    if !jit.is_empty() {
        file.set_meta(
            "amiberry_jit",
            if jit == "true" { "enabled" } else { "disabled" },
        );
    }

    if !file.get_meta_or("amiberry_model", "").is_empty() {
        return Ok(());
    }
    let model = file.get_meta_or("amiga_model", "");

    if !model.is_empty()
        && model != "date"
        && let Ok(machine) = model.parse::<Machine>()
    {
        file.set_machine(machine);
        info!("Machine: {machine:?} (from amiga_model)");
        return Ok(());
    }

    if aga {
        machine = Machine::A1200;
    }
    info!(">> CONFIG: aga: {aga} model: {model}");

    if model == "date"
        && let Ok(year) = file.get_meta_or("year", "").parse::<u32>()
    {
        if year < 1990 {
            machine = Machine::A500_KS12;
        } else if aga {
            if year >= 1995 {
                machine = Machine::A1230;
            } else {
                file.set_machine(Machine::A1200);
            }
        } else if year >= 1995 {
            machine = Machine::A500PLUS;
        }
        info!(">> YEAR: {year} machine: {machine:?}");
    }

    if file.has_tag("requires-kickstart-1.2") {
        machine = Machine::A500_KS12;
    }
    if file.has_tag("requires-1mb-chipmem") {
        machine = Machine::A500PLUS;
    }
    if file.has_tag("requires-68020") {
        machine = Machine::A1200;
    }
    if file.has_tag("requires-2mb-chipmem") {
        machine = Machine::A1200;
    }
    if file.has_tag("-fastmem") {
        machine = Machine::A1200FAST;
    }
    if file.has_tag("requires-68030") | file.has_tag("requires-fpu") {
        machine = Machine::A1230;
    }
    if file.has_tag("amos") {
        machine = Machine::A1230;
    }
    if file.has_tag("requires-68040") || file.has_tag("requires-68060") {
        machine = Machine::A1240;
    }
    file.set_machine(machine);

    info!("Machine: {machine:?}");

    Ok(())
}
