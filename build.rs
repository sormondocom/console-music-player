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
    // ── Android / Termux: stub out libc++_static ──────────────────────────────
    // Some transitive deps (e.g. idevice) emit `cargo:rustc-link-lib=c++_static`
    // even when no C++ code is compiled.  On Android the only C++ runtime
    // available is libc++_shared.so; the static archive does not exist.
    // We satisfy the linker by placing an empty archive named libc++_static.a in
    // OUT_DIR, then telling Cargo to search OUT_DIR first.  The real symbols come
    // from libc++_shared.so via -lc++_shared (injected via .cargo/config.toml).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let stub = std::path::Path::new(&out_dir).join("libc++_static.a");
        if !stub.exists() {
            // `ar rcs <archive>` with no object files creates a valid empty archive.
            let status = std::process::Command::new("ar")
                .args(["rcs", stub.to_str().unwrap()])
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => println!(
                    "cargo:warning=ar rcs for libc++_static.a stub exited with {s}"
                ),
                Err(e) => println!(
                    "cargo:warning=Could not create libc++_static.a stub (ar not found?): {e}"
                ),
            }
        }
        // Prepend OUT_DIR to the native search path so this stub is found before
        // any system paths are consulted.
        println!("cargo:rustc-link-search=native={out_dir}");
        // Pull in the real C++ runtime.
        println!("cargo:rustc-link-lib=c++_shared");
    }

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
