use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"));
    let cli_generated = out_dir.join("embedded_cli.rs");
    let gui_generated = out_dir.join("embedded_gui.rs");

    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var_os("CARGO_FEATURE_WINDOWS_LAUNCHER").is_none() {
        write_generated(&cli_generated, &[]).expect("write empty CLI embedded asset module");
        write_generated(&gui_generated, &[]).expect("write empty GUI embedded asset module");
        return;
    }

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        write_generated(&cli_generated, &[]).expect("write empty CLI embedded asset module");
        write_generated(&gui_generated, &[]).expect("write empty GUI embedded asset module");
        return;
    }

    #[cfg(windows)]
    compile_resources();

    let launcher_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is required"));
    let workspace_root = launcher_dir
        .parent()
        .and_then(Path::parent)
        .expect("launcher must be under the workspace crates directory")
        .to_path_buf();
    let target_root = match env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("target"),
    };
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let profile_dir = target_root.join(&profile);
    if !profile_dir.join("cbz-opt-core.exe").is_file()
        || !profile_dir.join("cbz-opt-gui-core.exe").is_file()
        || !profile_dir.join("dav1d.dll").is_file()
    {
        panic!(
            "launcher embedding inputs are missing; build cbz-opt-core.exe and cbz-opt-gui-core.exe first and stage dav1d.dll beside them; checked: {}",
            profile_dir.display()
        );
    }

    let cli_core = profile_dir.join("cbz-opt-core.exe");
    let gui_core = profile_dir.join("cbz-opt-gui-core.exe");
    let dav1d = profile_dir.join("dav1d.dll");
    for path in [&cli_core, &gui_core, &dav1d] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    write_generated(&cli_generated, &[cli_core, dav1d.clone()])
        .expect("write CLI embedded asset module");
    write_generated(&gui_generated, &[gui_core, dav1d]).expect("write GUI embedded asset module");
}

fn write_generated(destination: &Path, assets: &[PathBuf]) -> io::Result<()> {
    let mut source = String::from("pub static ASSETS: &[EmbeddedAsset] = &[\n");
    for path in assets {
        let bytes = fs::read(path)?;
        let hash = Sha256::digest(&bytes);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "asset has no UTF-8 name"))?;
        let escaped_path = path.to_string_lossy().replace('"', "\\\"");
        let hash_bytes = hash
            .iter()
            .map(|byte| format!("0x{byte:02x}"))
            .collect::<Vec<_>>()
            .join(", ");
        source.push_str(&format!(
            "    EmbeddedAsset {{ name: {name:?}, bytes: include_bytes!(r#\"{escaped_path}\"#), size: {}, sha256: [{hash_bytes}] }},\n",
            bytes.len()
        ));
    }
    source.push_str("];\n");
    fs::write(destination, source)
}

#[cfg(windows)]
fn compile_resources() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is required"));
    let icon = manifest_dir
        .join("..")
        .join("gui")
        .join("assets")
        .join("icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    let target_env = env::var("CARGO_CFG_TARGET_ENV").expect("CARGO_CFG_TARGET_ENV is required");

    compile_resource(&manifest_dir, &target_env, "cbz-opt", "cbz-opt.exe", None);
    compile_resource(
        &manifest_dir,
        &target_env,
        "cbz-opt-gui",
        "cbz-opt-gui.exe",
        Some(&icon),
    );
}

#[cfg(windows)]
fn compile_resource(
    manifest_dir: &Path,
    target_env: &str,
    bin_name: &str,
    original_filename: &str,
    icon: Option<&Path>,
) {
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is required");
    let mut resource = winres::WindowsResource::new();
    resource
        .set("ProductName", "CBZ Optimizer")
        .set("FileDescription", "CBZ Optimizer")
        .set("OriginalFilename", original_filename)
        .set("FileVersion", &version)
        .set("ProductVersion", &version);
    if let Some(icon) = icon {
        resource.set_icon(icon.to_string_lossy().as_ref());
    }

    let resource_script = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"))
        .join(format!("{bin_name}-resource.rc"));
    resource
        .write_resource_file(&resource_script)
        .unwrap_or_else(|error| panic!("failed to write {bin_name} resource script: {error}"));

    let resource_object =
        resource_script.with_extension(if target_env == "msvc" { "lib" } else { "o" });

    match target_env {
        "msvc" => {
            let rc = find_windows_sdk_tool("rc.exe").expect("Windows SDK rc.exe was not found");
            run_command(
                &rc,
                [
                    format!("/I{}", manifest_dir.display()),
                    format!("/fo{}", resource_object.display()),
                    resource_script.display().to_string(),
                ],
                "Windows resource compilation failed",
            );
        }
        "gnu" => {
            run_command(
                Path::new("windres"),
                [
                    resource_script.display().to_string(),
                    resource_object.display().to_string(),
                ],
                "GNU Windows resource compilation failed",
            );
        }
        _ => panic!("unsupported Windows target environment: {target_env}"),
    }

    println!(
        "cargo:rustc-link-arg-bin={bin_name}={}",
        resource_object.display()
    );
}

#[cfg(windows)]
fn find_windows_sdk_tool(name: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    }) {
        return Some(path);
    }

    let roots = [
        env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        env::var_os("ProgramFiles").map(PathBuf::from),
    ];
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => "x64",
    };

    roots.into_iter().flatten().find_map(|root| {
        let bin_root = root.join("Windows Kits").join("10").join("bin");
        let mut versions = fs::read_dir(bin_root)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        versions.sort_by(|left, right| right.cmp(left));
        versions
            .into_iter()
            .map(|version| version.join(architecture).join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(windows)]
fn run_command<I, S>(program: &Path, arguments: I, error_message: &str)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let status = std::process::Command::new(program)
        .args(arguments)
        .status()
        .unwrap_or_else(|error| panic!("{error_message}: {error}"));
    if !status.success() {
        panic!("{error_message}: {status}");
    }
}
