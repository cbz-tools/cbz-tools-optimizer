fn main() {
    #[cfg(target_os = "windows")]
    {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
        );
        let dav1d_dll = manifest_dir.join("../../third_party/dav1d/dav1d.dll");
        println!("cargo:rerun-if-changed={}", dav1d_dll.display());

        // OUT_DIR is target/<profile>/build/<crate-hash>/out. Put the runtime
        // dependency in the matching target/<profile> directory beside the EXE.
        let out_dir =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        let executable_dir = out_dir
            .ancestors()
            .nth(3)
            .expect("OUT_DIR has a target profile directory");
        let destination = executable_dir.join("dav1d.dll");
        std::fs::copy(&dav1d_dll, &destination).unwrap_or_else(|error| {
            panic!(
                "failed to copy {} to {}: {error}",
                dav1d_dll.display(),
                destination.display()
            )
        });

        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("winres compile failed");
    }
}
