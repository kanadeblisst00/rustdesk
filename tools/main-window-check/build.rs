fn main() {
    // Compile the production listener without pulling in video/audio backends.
    let path = "../../src/server.rs";
    let source = std::fs::read_to_string(path).unwrap();
    let start = source.find("#[cfg(all(target_os = \"macos\", feature = \"flutter\"))]\n#[tokio::main(flavor = \"current_thread\")]\npub async fn start_ipc_url_server").unwrap();
    let end = source[start..].find("\n#[cfg(any(target_os = \"macos\", target_os = \"linux\"))]\nasync fn wait_initial_config_sync").unwrap() + start;
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("url_server.rs"), &source[start..end]).unwrap();
    println!("cargo:rerun-if-changed={path}");
}
