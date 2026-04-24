#![allow(
    clippy::disallowed_methods,
    reason = "Build scripts run at compile time and read Cargo-provided env vars."
)]

fn main() {
    println!("cargo:rerun-if-changed=c/capture_argv.c");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        cc::Build::new()
            .file("c/capture_argv.c")
            .compile("capture_argv");
    }
}
