fn main() {
    // When the `tracker` feature is enabled, libopenmpt must be available.
    // Point the linker at our local `deps/` folder which contains the
    // pre-built `openmpt.lib` (extracted from the libopenmpt Windows dev
    // package at https://lib.openmpt.org/libopenmpt/download/).
    //
    // At runtime `libopenmpt.dll` must be discoverable — place it next to the
    // compiled binary or anywhere on PATH.
    #[cfg(feature = "tracker")]
    {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-search=native={}/deps", dir);
    }
}
