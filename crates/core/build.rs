fn main() {
    #[cfg(target_os = "windows")]
    {
        // UnRAR uses Windows registry, token, crypto, and ACL APIs.
        println!("cargo:rustc-link-lib=advapi32");
    }
}
