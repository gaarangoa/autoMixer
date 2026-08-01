use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use serde::Serialize;

static FFMPEG_PATH: OnceLock<PathBuf> = OnceLock::new();
static FFPROBE_PATH: OnceLock<PathBuf> = OnceLock::new();
static UV_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalToolStatus {
    pub name: String,
    pub available: bool,
    pub path: String,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub fn ffmpeg_path() -> &'static Path {
    FFMPEG_PATH
        .get_or_init(|| resolve_tool("ffmpeg", "AUTOMIXER_FFMPEG"))
        .as_path()
}

pub fn ffprobe_path() -> &'static Path {
    FFPROBE_PATH
        .get_or_init(|| resolve_tool("ffprobe", "AUTOMIXER_FFPROBE"))
        .as_path()
}

pub fn uv_path() -> &'static Path {
    UV_PATH
        .get_or_init(|| resolve_tool("uv", "AUTOMIXER_UV"))
        .as_path()
}

/// Check the external media executables used by recording, analysis, and rendering.
///
/// This deliberately reports problems instead of installing or modifying anything.
/// Finder-launched macOS apps receive a minimal PATH, so resolution also checks the
/// standard Homebrew locations and optional explicit environment overrides.
#[tauri::command]
pub fn check_external_dependencies() -> Vec<ExternalToolStatus> {
    vec![
        inspect_tool("uv", uv_path(), "AUTOMIXER_UV", "--version"),
        inspect_tool("ffmpeg", ffmpeg_path(), "AUTOMIXER_FFMPEG", "-version"),
        inspect_tool("ffprobe", ffprobe_path(), "AUTOMIXER_FFPROBE", "-version"),
    ]
}

pub fn log_external_dependency_status() {
    for status in check_external_dependencies() {
        if status.available {
            println!(
                "[dependencies] {} ready at {} ({})",
                status.name,
                status.path,
                status.version.as_deref().unwrap_or("version unavailable")
            );
        } else {
            eprintln!(
                "[dependencies] {} unavailable at {}: {}",
                status.name,
                status.path,
                status.error.as_deref().unwrap_or("unknown error")
            );
        }
    }
}

fn inspect_tool(
    name: &str,
    path: &Path,
    override_var: &str,
    version_arg: &str,
) -> ExternalToolStatus {
    let path_text = path.display().to_string();
    match Command::new(path).arg(version_arg).output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let version = stdout
                .lines()
                .chain(stderr.lines())
                .find(|line| !line.trim().is_empty())
                .map(|line| line.trim().to_string());
            ExternalToolStatus {
                name: name.to_string(),
                available: true,
                path: path_text,
                version,
                error: None,
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("the version check exited unsuccessfully");
            ExternalToolStatus {
                name: name.to_string(),
                available: false,
                path: path_text,
                version: None,
                error: Some(format!(
                    "{detail}. Install {name}, or set {override_var} to its executable path."
                )),
            }
        }
        Err(error) => ExternalToolStatus {
            name: name.to_string(),
            available: false,
            path: path_text,
            version: None,
            error: Some(format!(
                "{error}. Install {name}, or set {override_var} to its executable path."
            )),
        },
    }
}

fn resolve_tool(name: &str, override_var: &str) -> PathBuf {
    if let Some(path) = env::var_os(override_var).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    let executable_name = platform_executable_name(name);
    for candidate in bundled_candidates(&executable_name)
        .into_iter()
        .chain(user_candidates(&executable_name))
        .chain(system_candidates(&executable_name))
        .chain(path_candidates(&executable_name))
    {
        if candidate.is_file() {
            return candidate;
        }
    }

    // Keep the normal OS error when no candidate exists. Callers will include it in
    // their operation-specific error and the startup check provides remediation.
    PathBuf::from(executable_name)
}

fn user_candidates(executable_name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    env::var_os("HOME")
        .map(PathBuf::from)
        .into_iter()
        .map(move |home| home.join(".local").join("bin").join(executable_name))
}

fn bundled_candidates(executable_name: &str) -> Vec<PathBuf> {
    let Some(executable_dir) = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };

    let mut candidates = vec![
        executable_dir.join(executable_name),
        executable_dir.join("bin").join(executable_name),
    ];

    // A bundled macOS executable lives in AutoMixer.app/Contents/MacOS while
    // Tauri resources live in AutoMixer.app/Contents/Resources.
    if let Some(contents_dir) = executable_dir.parent() {
        candidates.push(contents_dir.join("Resources").join(executable_name));
        candidates.push(
            contents_dir
                .join("Resources")
                .join("bin")
                .join(executable_name),
        );
    }
    candidates
}

fn system_candidates(executable_name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    [
        // Homebrew on Apple Silicon and Intel Macs.
        Path::new("/opt/homebrew/bin").join(executable_name),
        Path::new("/usr/local/bin").join(executable_name),
        // Common system/package-manager locations on Linux and macOS.
        Path::new("/usr/bin").join(executable_name),
        Path::new("/bin").join(executable_name),
    ]
    .into_iter()
}

fn path_candidates(executable_name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .map(move |dir| dir.join(executable_name))
}

fn platform_executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::platform_executable_name;

    #[test]
    fn executable_name_matches_platform() {
        let expected = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        assert_eq!(platform_executable_name("ffmpeg"), expected);
    }
}
