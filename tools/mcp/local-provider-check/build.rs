fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let file = "../../../src/platform/local_agent_unix/macos.mm";
        cc::Build::new()
            .cpp(true)
            .flag("-std=c++17")
            .flag("-fobjc-arc")
            .file(file)
            .compile("mcp_local_mac");
        for framework in ["AppKit", "ApplicationServices", "Carbon"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
        println!("cargo:rerun-if-changed={file}");
    }
}
