//! Paths and isolated environments shared by commands.

use crate::manifest::Manifest;
use serde_json::Value;
use std::{
    env,
    error::Error,
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub struct Toolchain {
    pub root: PathBuf,
    pub m: Manifest,
}

impl Toolchain {
    pub fn tools(&self) -> PathBuf {
        self.root.join(".tools")
    }

    pub fn idf(&self) -> PathBuf {
        self.tools().join("esp-idf")
    }

    pub fn idf_home(&self) -> PathBuf {
        self.tools().join("espressif")
    }

    pub fn venv(&self) -> PathBuf {
        self.tools().join("idf-python")
    }

    pub fn verify(&self, file: &Path, want: &str) -> Result<(), String> {
        let got = self.output("sha256sum", &[&file.to_string_lossy()])?;
        if got.split_whitespace().next() == Some(want) {
            Ok(())
        } else {
            Err(format!("SHA-256 mismatch: {got}"))
        }
    }

    pub fn esp_emu(&self) -> PathBuf {
        let version = &self.m.esp_emu.version;
        self.tools().join("esp-emu").join(version).join("esp-emu")
    }

    pub fn smoke_source(&self) -> PathBuf {
        self.root.join("firmware/smoke")
    }

    pub fn smoke_build(&self) -> PathBuf {
        self.tools().join("smoke")
    }

    pub fn smoke_image(&self) -> PathBuf {
        self.smoke_build().join("merged.bin")
    }

    pub fn node_source(&self) -> PathBuf {
        self.root.join("firmware/node")
    }

    pub fn node_image(&self) -> PathBuf {
        self.tools().join("node/merged.bin")
    }

    /// Run with an allowlisted environment.
    pub fn plain(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear()
            .env("PATH", self.m.env.path.join(":"))
            .env("LC_ALL", "C.UTF-8")
            .env("ESP_EMU_DISABLE_AUTOUPDATES", "1");
        for key in [
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "TERM",
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
        ] {
            if let Some(v) = env::var_os(key) {
                c.env(key, v);
            }
        }
        c
    }

    /// Run a check with a 60-second timeout.
    pub fn command(&self, program: &Path) -> Command {
        let mut c = self.plain("/usr/bin/timeout");
        c.args(["--kill-after=5", "60"])
            .arg(program)
            // Keep checks read-only.
            .env("RUSTUP_AUTO_INSTALL", "0")
            .current_dir(&self.root)
            .stdin(Stdio::null());
        c
    }

    /// Run idf_tools.py with project-local storage.
    pub fn idf_tools_command(&self) -> Command {
        let mut c = self.plain(self.venv().join("bin/python"));
        c.arg("-B")
            .arg(self.idf().join("tools/idf_tools.py"))
            .arg("--non-interactive")
            .env("IDF_PATH", self.idf())
            .env("IDF_TOOLS_PATH", self.idf_home())
            .env("IDF_PYTHON_ENV_PATH", self.venv());
        c
    }

    /// Run with ESP-IDF and esp-emu on PATH.
    pub fn idf_command(&self, program: impl AsRef<OsStr>) -> Result<Command, Box<dyn Error>> {
        let out = self
            .idf_tools_command()
            .args(["export", "--format", "key-value"])
            .output()
            .map_err(|e| format!("Cannot run ESP-IDF Python ({e}). Run setup first"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("idf_tools.py export failed: {}", err.trim()).into());
        }
        let vars = parse_export(&String::from_utf8(out.stdout)?);
        let mut path = vec![self.venv().join("bin"), self.idf().join("tools")];
        path.extend(self.esp_emu().parent().map(PathBuf::from));
        path.extend(
            vars.iter()
                .filter(|(k, _)| k == "PATH")
                .flat_map(|(_, v)| env::split_paths(v).collect::<Vec<_>>())
                .filter(|p| p.is_absolute()),
        );
        path.extend(self.m.env.path.iter().map(PathBuf::from));

        let mut c = self.plain(program);
        c.envs(vars.iter().filter(|(k, _)| k != "PATH").cloned())
            .env("IDF_PATH", self.idf())
            .env("IDF_TOOLS_PATH", self.idf_home())
            .env("IDF_PYTHON_ENV_PATH", self.venv())
            .env(
                "IDF_COMPONENT_CACHE_PATH",
                self.idf_home().join("components"),
            )
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PATH", env::join_paths(path)?);
        Ok(c)
    }

    pub fn output(&self, program: impl AsRef<Path>, args: &[&str]) -> Result<String, String> {
        let program = program.as_ref();
        let out = self
            .command(program)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let detail = err.trim().lines().last().unwrap_or("no error output");
            Err(format!(
                "{}: {} ({})",
                program.display(),
                detail,
                out.status
            ))
        }
    }

    pub fn which(&self, name: &str) -> Option<PathBuf> {
        find_in(self.m.env.path.iter().map(PathBuf::from), name)
    }

    pub fn cargo_bin(&self, name: &str) -> Option<PathBuf> {
        let home = env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".cargo")))?;
        find_in([home.join("bin")].into_iter(), name)
    }
}

/// PATH of the calling shell.
/// The WSL version, or none outside WSL.
pub fn wsl() -> Option<u8> {
    wsl_version(&fs::read_to_string("/proc/sys/kernel/osrelease").ok()?)
}

fn wsl_version(kernel_release: &str) -> Option<u8> {
    let r = kernel_release.to_ascii_lowercase();
    let two = r.contains("microsoft-standard") || r.contains("wsl2");
    r.contains("microsoft").then_some(if two { 2 } else { 1 })
}

/// Windows programs can be started from Linux.
pub fn wsl_interop() -> bool {
    fs::read_to_string("/proc/sys/fs/binfmt_misc/WSLInterop")
        .is_ok_and(|s| s.starts_with("enabled"))
}

/// Rust target of the viewer under WSL2. setup adds it only there.
pub const WINDOWS_TARGET: &str = "x86_64-pc-windows-gnu";

/// Under WSL2 the viewer runs as a Windows program to use the Windows GPU driver.
pub fn windows_viewer() -> bool {
    wsl() == Some(2) && wsl_interop()
}

pub fn user_path() -> impl Iterator<Item = PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&path).collect::<Vec<_>>().into_iter()
}

fn parse_export(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().into(), v.trim().into()))
        .collect()
}

/// Require a uv environment with the exact Python version.
pub fn check_venv(cfg: &str, version: &str) -> Result<(), String> {
    let get = |key: &str| {
        cfg.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim())
    };
    if get("uv").is_none() {
        return Err("Virtual environment was not created by uv".into());
    }
    match get("version_info") {
        Some(v) if v == version => Ok(()),
        v => Err(format!(
            "Python {}, requires {version}",
            v.unwrap_or("unknown version")
        )),
    }
}

pub fn find_in(mut dirs: impl Iterator<Item = PathBuf>, name: &str) -> Option<PathBuf> {
    dirs.find_map(|d| {
        let p = d.join(name);
        let meta = p.metadata().ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(p)
    })
}

/// List required tools and recommended versions for the targets.
pub fn idf_tools(idf: &Path, targets: &[String]) -> Result<Vec<(String, String)>, String> {
    let text = fs::read_to_string(idf.join("tools/tools.json")).map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tools = json["tools"]
        .as_array()
        .ok_or("tools.json is missing tools")?;
    let mut list = vec![];
    for t in tools {
        let supported = t["supported_targets"].as_array().is_some_and(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .any(|s| s == "all" || targets.iter().any(|x| x == s))
        });
        if t["install"] != "always" || !supported {
            continue;
        }
        let name = t["name"]
            .as_str()
            .ok_or("Tool in tools.json is missing name")?;
        let version = t["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|v| v["status"] == "recommended")
            .and_then(|v| v["name"].as_str())
            .ok_or(format!("tools.json: {name} has no recommended version"))?;
        list.push((name.into(), version.into()));
    }
    if list.is_empty() {
        return Err("tools.json has no tools for the target chips".into());
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    #[test]
    fn wsl_version_from_kernel_release() {
        assert_eq!(wsl_version("6.18.33.2-microsoft-standard-WSL2"), Some(2));
        assert_eq!(wsl_version("4.4.0-19041-Microsoft"), Some(1));
        assert_eq!(wsl_version("6.8.0-45-generic"), None);
    }

    use super::*;

    #[test]
    fn venv_must_come_from_uv_with_exact_version() {
        assert!(check_venv("uv = 0.8.4\nversion_info = 3.13.5\n", "3.13.5").is_ok());
        assert!(check_venv("uv = 0.8.4\nversion_info = 3.13.50\n", "3.13.5").is_err());
        assert!(check_venv("version_info = 3.13.5\n", "3.13.5").is_err());
    }

    #[test]
    fn idf_tools_selects_required_for_targets() {
        let dir = env::temp_dir().join(format!("coldguard-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("tools")).unwrap();
        fs::write(
            dir.join("tools/tools.json"),
            r#"{"tools":[
              {"name":"a","install":"always","supported_targets":["esp32s3"],"versions":[{"name":"0","status":"supported"},{"name":"1","status":"recommended"}]},
              {"name":"b","install":"always","supported_targets":["all"],"versions":[{"name":"2","status":"recommended"}]},
              {"name":"c","install":"on_request","supported_targets":["all"],"versions":[]},
              {"name":"d","install":"always","supported_targets":["esp32p4"],"versions":[]}]}"#,
        )
        .unwrap();
        let got = idf_tools(&dir, &["esp32s3".into()]);
        let none = idf_tools(Path::new("/nonexistent"), &[]);
        fs::remove_dir_all(&dir).unwrap();
        let want = [("a", "1"), ("b", "2")].map(|(a, b)| (a.to_string(), b.to_string()));
        assert_eq!(got.unwrap(), want);
        assert!(none.is_err());
    }
}
