use std::path::Path;

// DLLs that must live next to cmp.exe at runtime.
// libopenmpt.dll depends on these companion DLLs — all come from the
// libopenmpt Windows dev package (bin/amd64/ inside libopenmpt-dev.zip).
const OPENMPT_DLLS: &[&str] = &[
    "libopenmpt.dll",
    "openmpt-mpg123.dll",
    "openmpt-ogg.dll",
    "openmpt-vorbis.dll",
    "openmpt-zlib.dll",
];

fn main() {
    if std::env::var("CARGO_FEATURE_TRACKER").is_ok() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();

        // Tell the linker where to find openmpt.lib.
        println!("cargo:rustc-link-search=native={manifest}/deps");

        // OUT_DIR is  …/target/{profile}/build/cmp-<hash>/out
        // The binary lives three levels up: …/target/{profile}/
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let bin_dir = Path::new(&out_dir)
            .ancestors()
            .nth(3)
            .expect("unexpected OUT_DIR depth");

        // Copy every companion DLL next to the binary so Windows finds them
        // via the exe-directory DLL search order.
        let deps = Path::new(&manifest).join("deps");
        for dll in OPENMPT_DLLS {
            let src = deps.join(dll);
            let dst = bin_dir.join(dll);
            if src.exists() {
                if let Err(e) = std::fs::copy(&src, &dst) {
                    println!("cargo:warning=Could not copy {dll} to {dst:?}: {e}");
                }
            } else {
                println!(
                    "cargo:warning=deps/{dll} not found — runtime DLL will be \
                     missing. Extract bin/amd64/{dll} from libopenmpt-dev.zip \
                     into deps/. See README.md for full instructions."
                );
            }
            println!("cargo:rerun-if-changed=deps/{dll}");
        }

        // On MSVC Windows, delay-load the main DLL so the process can start
        // even when the DLL is absent. main() then probes and prints a helpful
        // error instead of an OS-level crash dialog.
        #[cfg(all(target_os = "windows", target_env = "msvc"))]
        {
            println!("cargo:rustc-link-arg=/DELAYLOAD:libopenmpt.dll");
            println!("cargo:rustc-link-lib=delayimp");
        }

        println!("cargo:rerun-if-changed=deps/openmpt.lib");
    }
}
