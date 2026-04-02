use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../../apps/fabro-web/app");
    println!("cargo:rerun-if-changed=../../../apps/fabro-web/public");
    println!("cargo:rerun-if-changed=../../../apps/fabro-web/scripts/build.ts");
    println!("cargo:rerun-if-changed=../../../apps/fabro-web/index.template.html");
    println!("cargo:rerun-if-changed=../../../apps/fabro-web/package.json");

    let profile = std::env::var("PROFILE").unwrap_or_default();
    if profile != "release" {
        return;
    }

    let web_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../../apps/fabro-web");

    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(&web_dir)
        .status()
        .expect("failed to run `bun run build` for embedded web assets");

    if !status.success() {
        panic!("`bun run build` failed for embedded web assets");
    }
}
