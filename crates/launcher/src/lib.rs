use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

#[derive(Clone, Copy)]
pub struct EmbeddedAsset {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub size: u64,
    pub sha256: [u8; 32],
}

pub fn run_cli(assets: &[EmbeddedAsset]) -> i32 {
    match run_cli_impl(assets) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("CBZ Optimizer could not start.\n\n{error}");
            1
        }
    }
}

pub fn run_gui(assets: &[EmbeddedAsset]) -> i32 {
    match run_gui_impl(assets) {
        Ok(code) => code,
        Err(error) => {
            show_gui_error(&format!("CBZ Optimizer could not start.\n\n{error}"));
            1
        }
    }
}

#[cfg(windows)]
fn run_cli_impl(assets: &[EmbeddedAsset]) -> Result<i32, String> {
    let runtime_dir = ensure_runtime(assets)?;
    cleanup_old_runtime_directories(&runtime_dir);
    let core_path = runtime_dir.join("cbz-opt-core.exe");
    let status = std::process::Command::new(core_path)
        .args(std::env::args_os().skip(1))
        .status()
        .map_err(|error| format!("failed to start core executable: {error}"))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(not(windows))]
fn run_cli_impl(_assets: &[EmbeddedAsset]) -> Result<i32, String> {
    Err("the bundled launcher is only supported on Windows".to_string())
}

#[cfg(windows)]
fn run_gui_impl(assets: &[EmbeddedAsset]) -> Result<i32, String> {
    let runtime_dir = ensure_runtime(assets)?;
    cleanup_old_runtime_directories(&runtime_dir);
    let core_path = runtime_dir.join("cbz-opt-gui-core.exe");
    let config_dir = launcher_executable_dir()?;
    std::process::Command::new(core_path)
        .args(std::env::args_os().skip(1))
        .env("CBZ_OPT_GUI_CONFIG_DIR", config_dir)
        .spawn()
        .map_err(|error| format!("failed to start GUI core executable: {error}"))?;
    Ok(0)
}

#[cfg(windows)]
fn launcher_executable_dir() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not determine launcher executable path: {error}"))?;
    executable
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "launcher executable has no parent directory".to_string())
}

#[cfg(not(windows))]
fn run_gui_impl(_assets: &[EmbeddedAsset]) -> Result<i32, String> {
    Err("the bundled launcher is only supported on Windows".to_string())
}

#[cfg(windows)]
const APP_ID: &str = "cbz-tools-optimizer";
#[cfg(windows)]
const RUNTIME_SUBDIRECTORY: &str = "runtime";
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

#[cfg(windows)]
fn cleanup_old_runtime_directories(current_runtime_dir: &Path) {
    let Some(runtime_root) = current_runtime_dir.parent() else {
        return;
    };
    let Some(current_version) = parse_release_version(env!("CARGO_PKG_VERSION")) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(runtime_root) else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir()
            || metadata.is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            continue;
        }

        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(version) = parse_release_version(name) else {
            continue;
        };
        if version < current_version {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(windows)]
fn parse_release_version(name: &str) -> Option<(u64, u64, u64)> {
    let components: Vec<_> = name.split('.').collect();
    if components.len() != 3 {
        return None;
    }

    let [major, minor, patch] = components.as_slice() else {
        return None;
    };
    Some((
        parse_release_component(major)?,
        parse_release_component(minor)?,
        parse_release_component(patch)?,
    ))
}

#[cfg(windows)]
fn parse_release_component(component: &str) -> Option<u64> {
    if component == "0" {
        return Some(0);
    }

    if component.is_empty()
        || component.starts_with('0')
        || !component.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    component.parse().ok()
}

#[cfg(windows)]
fn ensure_runtime(assets: &[EmbeddedAsset]) -> Result<PathBuf, String> {
    let runtime_dir = runtime_dir();
    std::fs::create_dir_all(&runtime_dir)
        .map_err(|error| format!("could not create runtime directory: {error}"))?;

    let missing = assets
        .iter()
        .copied()
        .filter(|asset| !is_valid_asset(&runtime_dir.join(asset.name), asset))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(runtime_dir);
    }

    let temporary_dir = make_temporary_directory(&runtime_dir)?;
    let result = (|| {
        for asset in &missing {
            let temporary_path = temporary_dir.join(asset.name);
            let mut file = std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(|error| format!("could not stage {}: {error}", asset.name))?;
            use std::io::Write;
            file.write_all(asset.bytes)
                .map_err(|error| format!("could not write {}: {error}", asset.name))?;
            file.sync_all()
                .map_err(|error| format!("could not flush {}: {error}", asset.name))?;
        }

        for asset in &missing {
            let temporary_path = temporary_dir.join(asset.name);
            let destination = runtime_dir.join(asset.name);
            if let Err(error) = replace_file_atomically(&temporary_path, &destination) {
                if is_valid_asset(&destination, asset) {
                    continue;
                }
                return Err(format!(
                    "could not install {} at {}: {error}; destination revalidation failed (missing or invalid)",
                    asset.name,
                    destination.display()
                ));
            }
        }

        for asset in assets {
            if !is_valid_asset(&runtime_dir.join(asset.name), asset) {
                return Err(format!("installed asset failed validation: {}", asset.name));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&temporary_dir);
    result.map(|()| runtime_dir)
}

#[cfg(windows)]
fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(APP_ID)
        .join(RUNTIME_SUBDIRECTORY)
        .join(env!("CARGO_PKG_VERSION"))
}

#[cfg(windows)]
fn is_valid_asset(path: &std::path::Path, asset: &EmbeddedAsset) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.len() != asset.size {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).as_slice() == asset.sha256.as_slice()
}

#[cfg(windows)]
fn make_temporary_directory(runtime_dir: &std::path::Path) -> Result<PathBuf, String> {
    let pid = std::process::id();
    for attempt in 0..32u32 {
        let candidate = runtime_dir.join(format!(".extract-{pid}-{attempt}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create temporary runtime directory: {error}"
                ));
            }
        }
    }
    Err("could not allocate a unique temporary runtime directory".to_string())
}

#[cfg(windows)]
fn replace_file_atomically(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    let source = wide_null(source.as_os_str());
    let destination = wide_null(destination.as_os_str());
    // SAFETY: both paths are valid, NUL-terminated UTF-16 strings owned for
    // the duration of this call; the API only replaces the named file.
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn show_gui_error(message: &str) {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::ptr::null_mut;
        let text = wide_null(OsStr::new(message));
        let title = wide_null(OsStr::new("CBZ Optimizer"));
        // SAFETY: both UTF-16 strings remain alive for the duration of the call.
        unsafe {
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
}

#[cfg(windows)]
const MB_OK: u32 = 0x0000_0000;
#[cfg(windows)]
const MB_ICONERROR: u32 = 0x0000_0010;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        window: *mut core::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        type_: u32,
    ) -> i32;
}
