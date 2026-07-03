fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    link_with_pkg_config_or_fallback("libvirt", "virt");
    link_with_pkg_config_or_fallback("libvirt-qemu", "virt-qemu");
}

fn link_with_pkg_config_or_fallback(package: &str, library: &str) {
    if pkg_config::Config::new().probe(package).is_err() {
        println!("cargo:rustc-link-lib={library}");
    }
}
