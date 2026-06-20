fn main() {
    // Link against libwayland-client.so for the raw FFI declarations
    // in panel.rs and input_method.rs (wl_display_connect, wl_registry_bind,
    // wl_compositor_create_surface, etc).
    println!("cargo:rustc-link-lib=wayland-client");
}
