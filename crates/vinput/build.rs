//! Embeds the application icon and version details into the Windows executable,
//! so Vinput looks like an app in the Start menu, search and Task Manager.

/// Libraries of Windows that are loaded when first called instead of when the
/// process starts, because the background process seldom needs them: Media
/// Foundation saves voice memos, and DXGI names the graphics card.
const DELAYED: [&str; 3] = ["mfplat.dll", "mfreadwrite.dll", "dxgi.dll"];

fn main() {
    println!("cargo:rerun-if-changed=../../assets/vinput.ico");
    println!("cargo:rerun-if-changed=../../assets/vinput-listening.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for library in DELAYED {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{library}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    let mut resources = winresource::WindowsResource::new();
    resources.set_icon("../../assets/vinput.ico");
    // The tray shows this one, the logo with a red dot, while Vinput listens.
    resources.set_icon_with_id("../../assets/vinput-listening.ico", "2");
    resources.set("ProductName", "Vinput");
    resources.set("FileDescription", "Vinput");
    if let Err(err) = resources.compile() {
        // Without the Windows SDK's resource compiler the build still works; the
        // executable just has no icon.
        println!("cargo:warning=could not embed the Vinput icon: {err}");
    }
}
