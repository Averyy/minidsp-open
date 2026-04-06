fn main() {
    embuild::espidf::sysenv::output();

    // Register ESP-IDF cfg flags so rustc doesn't warn about unexpected cfgs
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_espressif__mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_httpd_ws_support)");
}
