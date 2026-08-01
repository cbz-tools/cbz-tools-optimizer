fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rustc-link-lib=advapi32");
        let manifest_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
        );
        let unrar_dll = manifest_dir.join("../../third_party/unrar/x64/UnRAR64.dll");
        println!("cargo:rerun-if-changed={}", unrar_dll.display());

        let out_dir =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        let executable_dir = out_dir
            .ancestors()
            .nth(3)
            .expect("OUT_DIR has a target profile directory");
        let destination = executable_dir.join("UnRAR64.dll");
        std::fs::copy(&unrar_dll, &destination).unwrap_or_else(|error| {
            panic!(
                "failed to copy {} to {}: {error}",
                unrar_dll.display(),
                destination.display()
            )
        });
    }
}
