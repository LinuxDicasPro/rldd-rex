use glob::glob;
use goblin::elf::Elf;
use goblin::elf::header::*;
use memmap2::Mmap;
use std::collections::HashSet;
use std::fs;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfArch {
    Elf32,
    Elf64,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfMachine {
    X86,
    X86_64,
    Arm32,
    Arm64,
    Mips,
    PowerPC,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfType {
    Static,
    Dynamic,
    Pie,
    Invalid,
}

#[derive(Debug)]
pub struct RlddRexInfo {
    pub arch: ElfArch,
    pub elf_type: ElfType,
    pub deps: Vec<(String, String)>,
}

#[cfg(feature = "enable_ld_library_path")]
fn is_same_arch(arch: ElfArch, sub_elf: &Elf) -> bool {
    match arch {
        ElfArch::Elf32 => !sub_elf.is_64,
        ElfArch::Elf64 => sub_elf.is_64,
        ElfArch::Unknown => true, // fallback
    }
}

impl ElfType {
    pub fn is_static(&self) -> bool {
        *self == ElfType::Static
    }
    pub fn is_dynamic(&self) -> bool {
        *self == ElfType::Dynamic
    }
    pub fn is_pie(&self) -> bool {
        *self == ElfType::Pie
    }
    pub fn is_valid(&self) -> bool {
        *self != ElfType::Invalid
    }
}

fn get_elf_type(elf: &Elf) -> ElfType {
    match elf.header.e_type {
        ET_EXEC => {
            if elf.dynamic.is_some() {
                ElfType::Dynamic
            } else {
                ElfType::Static
            }
        }
        ET_DYN => {
            if elf.interpreter.is_some() {
                ElfType::Pie
            } else {
                ElfType::Dynamic
            }
        }
        _ => ElfType::Invalid, // ET_CORE
    }
}

fn machine_from_e_machine(e_machine: u16) -> ElfMachine {
    match e_machine {
        EM_386 => ElfMachine::X86,
        EM_X86_64 => ElfMachine::X86_64,
        EM_ARM => ElfMachine::Arm32,
        EM_AARCH64 => ElfMachine::Arm64,
        EM_MIPS => ElfMachine::Mips,
        EM_PPC => ElfMachine::PowerPC,
        _ => ElfMachine::Unknown,
    }
}

#[cfg(any(target_os = "linux", target_os = "solaris"))]
fn read_ld_so_conf() -> io::Result<Vec<PathBuf>> {
    let mut collected = Vec::new();
    let mut seen = HashSet::new();

    fn process_file(path: &Path, collected: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
        if let Ok(content) = fs::read_to_string(path) {
            for line in content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                if let Some(rest) = line.strip_prefix("include") {
                    let pattern = rest.trim();
                    if let Ok(entries) = glob(pattern) {
                        for entry in entries.flatten().filter(|e| e.is_file()) {
                            process_file(&entry, collected, seen);
                        }
                    } else {
                        eprintln!("Glob error '{}'", pattern);
                    }
                } else {
                    let dir = PathBuf::from(line);
                    if dir.exists() && dir.is_dir() && seen.insert(dir.clone()) {
                        collected.push(dir);
                    }
                }
            }
        } else {
            eprintln!("Fail to read {:?}", path);
        }
    }

    let base = Path::new("/etc/ld.so.conf");
    if base.exists() {
        process_file(base, &mut collected, &mut seen);
    }

    Ok(collected)
}

fn default_dirs_for_arch_and_machine(elf_arch: ElfArch, machine: ElfMachine) -> Vec<PathBuf> {
    let mut dirs = match elf_arch {
        ElfArch::Elf32 => vec![
            PathBuf::from("/lib"),
            PathBuf::from("/usr/lib"),
            PathBuf::from("/lib32"),
            PathBuf::from("/usr/lib32"),
            #[cfg(target_os = "solaris")]
            PathBuf::from("/usr/lib/32"),
        ],
        ElfArch::Elf64 => vec![
            PathBuf::from("/lib64"),
            PathBuf::from("/usr/lib64"),
            #[cfg(target_os = "solaris")]
            PathBuf::from("/usr/lib/64"),
        ],
        ElfArch::Unknown => vec![],
    };

    let machine_dirs = match machine {
        ElfMachine::PowerPC => match elf_arch {
            ElfArch::Elf32 => vec![
                PathBuf::from("/lib/powerpc-linux-gnu"),
                PathBuf::from("/usr/lib/powerpc-linux-gnu"),
            ],
            ElfArch::Elf64 => vec![
                PathBuf::from("/lib/powerpc64-linux-gnu"),
                PathBuf::from("/usr/lib/powerpc64-linux-gnu"),
            ],
            _ => vec![],
        },
        ElfMachine::Mips => match elf_arch {
            ElfArch::Elf32 => vec![
                PathBuf::from("/lib/mips-linux-gnu"),
                PathBuf::from("/usr/lib/mips-linux-gnu"),
            ],
            ElfArch::Elf64 => vec![
                PathBuf::from("/lib/mips64-linux-gnu"),
                PathBuf::from("/usr/lib/mips64-linux-gnu"),
            ],
            _ => vec![],
        },
        ElfMachine::Arm32 => vec![
            PathBuf::from("/lib/arm-linux-gnueabihf"),
            PathBuf::from("/usr/lib/arm-linux-gnueabihf"),
        ],
        ElfMachine::Arm64 => vec![
            PathBuf::from("/lib/aarch64-linux-gnu"),
            PathBuf::from("/usr/lib/aarch64-linux-gnu"),
        ],
        ElfMachine::X86 => match elf_arch {
            ElfArch::Elf32 => vec![
                PathBuf::from("/lib/i386-linux-gnu"),
                PathBuf::from("/usr/lib/i386-linux-gnu"),
            ],
            _ => vec![],
        },
        ElfMachine::X86_64 => match elf_arch {
            ElfArch::Elf64 => vec![
                PathBuf::from("/lib/x86_64-linux-gnu"),
                PathBuf::from("/usr/lib/x86_64-linux-gnu"),
            ],
            ElfArch::Elf32 => vec![
                PathBuf::from("/lib/i386-linux-gnu"),
                PathBuf::from("/usr/lib/i386-linux-gnu"),
            ],
            _ => vec![],
        },
        ElfMachine::Unknown => vec![],
    };

    dirs.extend(machine_dirs);
    dirs
}

fn build_search_dirs(elf: &Elf, arch: ElfArch, machine: ElfMachine) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/lib"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/local/lib"),
        PathBuf::from("/usr/libexec"),
        PathBuf::from("/libexec"),
    ];

    #[cfg(feature = "enable_ld_library_path")]
    if let Ok(ld_path) = std::env::var("LD_LIBRARY_PATH") {
        for p in ld_path.split(':') {
            let seg = if p.is_empty() { "." } else { p };
            dirs.push(PathBuf::from(seg));
        }
    }

    let is_musl = if let Some(interp) = elf.interpreter {
        interp.contains("musl")
    } else {
        false
    };

    if is_musl {
        let musl_conf = Path::new("/etc/ld-musl-x86_64.path");
        if musl_conf.exists() {
            if let Ok(content) = fs::read_to_string(musl_conf) {
                for line in content.lines() {
                    let trim = line.trim();
                    if !trim.is_empty() {
                        dirs.push(PathBuf::from(trim));
                    }
                }
            }
        }
    } else {
        #[cfg(any(target_os = "linux", target_os = "solaris"))]
        if let Err(e) = read_ld_so_conf().map(|ld_dirs| dirs.extend(ld_dirs)) {
            eprintln!("Error reading ld.so.conf: {}", e);
        }
        dirs.extend(default_dirs_for_arch_and_machine(arch, machine));
    }

    let mut uniq = Vec::new();
    let mut seen = HashSet::new();
    for d in dirs {
        let path = d.canonicalize().unwrap_or(d);
        if seen.insert(path.clone()) {
            uniq.push(path);
        }
    }

    uniq
}

fn find_library(lib: &str, search_dirs: &[PathBuf], paths: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs = search_dirs.to_vec();
    dirs.extend(paths.iter().map(PathBuf::from));

    for dir in dirs {
        let candidate = dir.join(lib);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn resolve_origin(bin_path: &Path, entry: &str) -> PathBuf {
    if entry.starts_with("$ORIGIN") {
        let rel = entry.trim_start_matches("$ORIGIN");
        bin_path.parent().unwrap_or(Path::new("/")).join(rel)
    } else {
        PathBuf::from(entry)
    }
}

fn open_and_map(path: &impl AsRef<Path>) -> io::Result<Mmap> {
    let file = File::open(path)?;
    let map = unsafe { Mmap::map(&file)? };
    Ok(map)
}

fn empty_info() -> RlddRexInfo {
    RlddRexInfo {
        arch: ElfArch::Unknown,
        elf_type: ElfType::Invalid,
        deps: Vec::new(),
    }
}

fn extra_lib_dirs_for_bin(path: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let lib_names = ["lib", "lib64", "libs"];
    let real_bin = match fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => path.to_path_buf(),
    };

    let bin_dir = match real_bin.parent() {
        Some(p) => p.to_path_buf(),
        None => return dirs,
    };

    dirs.push(bin_dir.clone());

    for lib in lib_names {
        dirs.push(bin_dir.join(lib));
    }

    if bin_dir.file_name().map_or(false, |f| f == "bin") {
        if let Some(parent) = bin_dir.parent() {
            for lib in lib_names {
                dirs.push(parent.join(lib));
            }
        }
    }

    dirs
}

fn inner(
    path: &Path,
    elf: &Elf,
    visited: &mut HashSet<(u64, u64)>,
    seen_libs: &mut HashSet<String>,
    res: &mut Vec<(String, String)>,
    dirs: &[PathBuf],
    arch: ElfArch,
    d: usize,
) -> io::Result<()> {
    if d > MAX_DEPTH {
        eprintln!("Warning: max recursion depth at {:?}", path);
        return Ok(());
    }

    if let Ok(meta) = fs::metadata(path) {
        let key = (meta.dev(), meta.ino());
        if !visited.insert(key) {
            return Ok(());
        }
    } else {
        eprintln!("Error access {:?}", path);
        return Ok(());
    }

    let deps: Vec<_> = elf.libraries.iter().map(ToString::to_string).collect();
    let paths: Vec<_> = elf
        .rpaths
        .iter()
        .chain(&elf.runpaths)
        .map(|s| resolve_origin(path, s))
        .collect();

    for dep in deps {
        if !seen_libs.insert(dep.clone()) {
            continue;
        }

        let display = find_library(&dep, dirs, &paths)
            .map(|found| {
                if let Ok(map) = open_and_map(&found) {
                    if let Ok(s_elf) = Elf::parse(&map) {
                        #[cfg(feature = "enable_ld_library_path")]
                        if !is_same_arch(arch, &s_elf) {
                            return "arch mismatch".into(); // Retorna aqui direto
                        }

                        if let Err(e) =
                            inner(&found, &s_elf, visited, seen_libs, res, dirs, arch, d + 1)
                        {
                            eprintln!("Recursive error {:?}: {:?}", found, e);
                        }
                    }
                }
                found.display().to_string()
            })
            .unwrap_or_else(|| "not found".into());

        res.push((dep, display));
    }

    Ok(())
}

pub fn rldd_rex<P: AsRef<Path> + std::fmt::Debug>(path: P) -> io::Result<RlddRexInfo> {
    let (mut libs, mut visited) = (HashSet::new(), HashSet::new());
    let mut res = Vec::new();

    let map = match open_and_map(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Fail to open or map {:?}: {}", path, e);
            return Ok(empty_info());
        }
    };

    let elf = match Elf::parse(&map) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Fail to parser ELF {:?}: {}", path, e);
            return Ok(empty_info());
        }
    };

    let arch = [ElfArch::Elf32, ElfArch::Elf64][elf.is_64 as usize];
    let machine = machine_from_e_machine(elf.header.e_machine);
    let elf_type = get_elf_type(&elf);

    if let Some(interp) = elf.interpreter {
        if interp.contains("musl") {
            let interp_path = PathBuf::from(interp);

            let resolved_interp = if interp_path.exists() {
                interp_path.canonicalize().unwrap_or(interp_path.clone())
            } else {
                interp_path.clone()
            };

            let lib_name = interp_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(interp)
                .to_string();

            res.push((lib_name.clone(), resolved_interp.display().to_string()));
            libs.insert(lib_name);
        }
    }

    let mut search_dirs = build_search_dirs(&elf, arch, machine);
    search_dirs.extend(extra_lib_dirs_for_bin(path.as_ref()));

    inner(
        path.as_ref(),
        &elf,
        &mut visited,
        &mut libs,
        &mut res,
        &search_dirs,
        arch,
        0,
    )?;

    Ok(RlddRexInfo {
        arch,
        elf_type,
        deps: res,
    })
}

#[cfg(test)]
mod tests;
