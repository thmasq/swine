use std::env;
use std::path::PathBuf;

fn main() -> Result<(), pkg_config::Error> {
    println!("cargo::rerun-if-changed=wrapper.h");
    println!("cargo::rerun-if-env-changed=KRUN_STATIC");

    let is_static = env::var("KRUN_STATIC").is_ok();

    let library = if is_static {
        let output = std::process::Command::new("pkg-config")
            .env("PKG_CONFIG_ALLOW_SYSTEM_LIBS", "1")
            .args(&["--libs", "--static", "libkrun"])
            .output()
            .expect("Failed to run pkg-config");

        let libs_str = String::from_utf8_lossy(&output.stdout);

        println!("cargo:rustc-link-search=native=/usr/lib");
        println!("cargo:rustc-link-search=native=/usr/lib64");

        for token in libs_str.split_whitespace() {
            if let Some(path) = token.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={}", path);
            } else if let Some(lib) = token.strip_prefix("-l") {
                if lib == "krun" || lib == "krunfw" {
                    println!("cargo:rustc-link-lib=static={}", lib);
                } else {
                    println!("cargo:rustc-link-lib=dylib={}", lib);
                }
            }
        }

        pkg_config::Config::new()
            .cargo_metadata(false)
            .probe("libkrun")?
    } else {
        pkg_config::probe_library("libkrun")?
    };

    let bindings = bindgen::Builder::default()
        .clang_args(
            library
                .include_paths
                .iter()
                .map(|path| format!("-I{}", path.to_string_lossy())),
        )
        .clang_arg("-fretain-comments-from-system-headers")
        .header("wrapper.h")
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    Ok(())
}
