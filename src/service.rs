#[cfg(not(feature = "mcp-isolated"))]
use librustdesk::*;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(all(target_os = "macos", not(feature = "mcp-isolated")))]
fn main() {
    #[cfg(feature = "mcp")]
    crate::common::load_custom_client();
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--write-plists" {
        if let Err(e) = librustdesk::platform::write_plists() {
            eprintln!("Failed to write plists: {}", e);
            std::process::exit(1);
        }
        std::process::exit(0);
    }
    crate::common::load_custom_client();
    hbb_common::init_log(false, "service");
    crate::start_os_service();
}

#[cfg(all(target_os = "macos", feature = "mcp-isolated"))]
fn main() {
    eprintln!("System services are disabled in RustDeskMCPTest");
    std::process::exit(2);
}
