use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ui_out = out.join("ui");
    std::fs::create_dir_all(&ui_out).unwrap();

    // BLUEPRINT_COMPILER may hold a command with arguments, e.g. "python3.12 /usr/bin/blueprint-compiler".
    let compiler = std::env::var("BLUEPRINT_COMPILER").unwrap_or_else(|_| "blueprint-compiler".into());
    let mut parts = compiler.split_whitespace();
    let mut cmd = Command::new(parts.next().unwrap());
    cmd.args(parts).arg("batch-compile").arg(&ui_out).arg("data/ui");
    let blps: Vec<_> = std::fs::read_dir("data/ui")
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "blp"))
        .collect();
    for p in &blps {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    let status = cmd.args(&blps).status().expect("blueprint-compiler not found (set BLUEPRINT_COMPILER)");
    assert!(status.success(), "blueprint-compiler failed");

    println!("cargo:rerun-if-env-changed=BLUEPRINT_COMPILER");
    glib_build_tools::compile_resources(
        &[ui_out.to_str().unwrap(), "data"],
        "data/resources.gresource.xml",
        "tm02link.gresource",
    );
}
