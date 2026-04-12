#[expect(
    clippy::disallowed_methods,
    reason = "Build scripts run outside Tokio and need a synchronous git probe for the embedded build SHA."
)]
fn main() {
    println!("cargo:rerun-if-changed=../../../.git/HEAD");

    let sha = std::process::Command::new("git")
        .args(["rev-list", "-1", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let short_sha = if sha.len() >= 7 { &sha[..7] } else { &sha };
    println!("cargo:rustc-env=FABRO_GIT_SHA={short_sha}");

    let build_date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    println!("cargo:rustc-env=FABRO_BUILD_DATE={build_date}");
}
