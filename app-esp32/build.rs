fn main() {
    embuild::espidf::sysenv::output();

    // Apply the HTTP origin guard and the IDF 6 WebSocket callbacks.
    println!("cargo::rustc-link-arg-bin=minidsp-esp32=-Wl,--wrap=httpd_register_uri_handler");

    // Register ESP-IDF cfg flags so rustc doesn't warn about unexpected cfgs
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_espressif__mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_httpd_ws_support)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_heap_tracing_standalone)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_lwip_ipv6)");

    // Identify the exact build in GET /firmware: CI passes MINIDSP_BUILD_REV,
    // local builds fall back to `git describe`.
    println!("cargo::rerun-if-env-changed=MINIDSP_BUILD_REV");
    println!("cargo::rerun-if-changed=../.git/HEAD");
    println!("cargo::rerun-if-changed=../.git/index");
    let rev = std::env::var("MINIDSP_BUILD_REV")
        .ok()
        .filter(|rev| !rev.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["describe", "--always", "--dirty", "--exclude", "*"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|rev| rev.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo::rustc-env=MINIDSP_BUILD_REV={rev}");
}
