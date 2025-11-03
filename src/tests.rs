use super::*;

#[test]
fn test_verbose_deps() -> Result<(), Box<dyn std::error::Error>> {
    let select = 1;

    #[cfg(feature = "enable_ld_library_path")]
    {
        let ld_path = "/lib64:/usr/lib64:/usr/local/lib64:/lib:/usr/lib:/usr/local/lib";
        unsafe { std::env::set_var("LD_LIBRARY_PATH", ld_path); }
    }

    let path = match select {
        1 => "/bin/ls",
        2 => "/etc/os-release",
        3 => "filenotfound",
        4 => "/usr/lib/libcuda.so.570.169",
        5 => "/usr/lib64/libcuda.so.570.169",
        6 => "/usr/bin/ocenaudio",
        _ => panic!("Seleção inválida"),
    };

    let deps = rldd_rex(path)?;
    let (mut dnf, mut df) = (0, 0);

    println!("\nRLDD-REX TEST\n");
    match deps.elf_type {
        ElfType::Dynamic => println!("Dynamic: depends on shared libs"),
        ElfType::Static => println!("Static: all libs included"),
        ElfType::Pie => println!("PIE: position independent executable"),
        ElfType::Invalid => println!("Invalid ELF"),
    }

    match deps.arch {
        ElfArch::Elf32 => println!("Elf32: Arquitecture 32-bit"),
        ElfArch::Elf64 => println!("Elf64: Arquitecture 64-bit"),
        ElfArch::Unknown => println!("Unknown ELF")
    }

    println!("\nRecursive dependencies for {}:", path);
    for (i, (lib, path_or_status)) in deps.deps.iter().enumerate() {
        let num = i + 1;
        let colorized = if path_or_status == "not found" || path_or_status == "arch mismatch" {
            format!("\x1b[1;31m{}\x1b[0m", path_or_status)
        } else if path_or_status.starts_with("/lib") || path_or_status.starts_with("/usr") {
            format!("\x1b[1;32m{}\x1b[0m", path_or_status)
        } else {
            format!("\x1b[1;33m{}\x1b[0m", path_or_status)
        };

        println!("{num}. {} => {}", lib, colorized);

        match path_or_status.as_str() {
            "not found" | "arch mismatch" => dnf += 1,
            _ => df += 1,
        }
    }
    println!("\nDependencies found: {df}");
    println!("Dependencies not found: {dnf}\n");
    Ok(())
}
