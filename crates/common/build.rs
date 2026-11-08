fn main() {
    println!("cargo:rerun-if-env-changed=STREAMLOOP_BUILD_VERSION");
    let version = std::env::var("STREAMLOOP_BUILD_VERSION").unwrap_or_else(|_| std::env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo:rustc-env=STREAMLOOP_VERSION={version}");
}
