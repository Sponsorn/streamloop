//! Gives streamloop.exe its icon and version resource on Windows.

const ICON: &str = "assets/streamloop.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    // The build script runs on the host; the target decides.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    // GPUI registers its window class with icon resource 1, so the window and the taskbar show this icon too.
    resource.set_icon_with_id(ICON, "1");
    // Windows' notifications name the app by its FileDescription. The version is the package's.
    resource.set("FileDescription", "StreamLoop");
    resource.set("ProductName", "StreamLoop");
    resource
        .compile()
        .expect("embed the icon and version resource in streamloop.exe");
}
