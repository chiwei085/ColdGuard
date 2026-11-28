//! Read-only toolchain checks with evidence and fixes.

use crate::{
    smoke,
    toolchain::{
        Toolchain, WINDOWS_TARGET, check_venv, find_in, idf_tools, user_path, windows_viewer, wsl,
        wsl_interop,
    },
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    env, fs,
    net::{Ipv4Addr, TcpListener},
    path::Path,
};

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Serialize)]
pub struct Check {
    layer: &'static str,
    id: String,
    pub status: Status,
    evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

struct Doctor<'a> {
    t: &'a Toolchain,
    layer: &'static str,
    checks: Vec<Check>,
}

pub fn run(t: &Toolchain) -> Vec<Check> {
    let mut d = Doctor {
        t,
        layer: "Host",
        checks: vec![],
    };
    d.host();
    d.layer = "Tools";
    d.rust();
    d.python();
    d.esp_idf();
    d.esp_emu();
    d.windows_target();
    d.layer = "Isolation";
    d.isolation();
    d.layer = "Runtime";
    d.runtime();
    d.layer = "Project";
    d.project();
    d.layer = "End-to-end";
    d.end_to_end();
    d.checks
}

pub fn print(checks: &[Check]) {
    let mut layer = "";
    for c in checks {
        if c.layer != layer {
            layer = c.layer;
            println!("\n{layer}");
        }
        let mark = match c.status {
            Status::Ok => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
        };
        println!("  {mark} {}: {}", c.id, c.evidence);
        if let Some(fix) = &c.fix {
            println!("      → {fix}");
        }
    }
    let n = |s| checks.iter().filter(|c| c.status == s).count();
    println!(
        "\n{} passed, {} warnings, {} failed",
        n(Status::Ok),
        n(Status::Warn),
        n(Status::Fail)
    );
}

impl Doctor<'_> {
    fn at_least(&self, have: &str, want: &str) -> bool {
        self.t
            .output("dpkg", &["--compare-versions", have, "ge", want])
            .is_ok()
    }

    fn add(
        &mut self,
        id: impl Into<String>,
        status: Status,
        evidence: impl Into<String>,
        fix: Option<String>,
    ) {
        let fix = fix.filter(|_| status != Status::Ok);
        self.checks.push(Check {
            layer: self.layer,
            id: id.into(),
            status,
            evidence: evidence.into(),
            fix,
        });
    }

    fn host(&mut self) {
        let h = &self.t.m.host;
        let os = fs::read_to_string("/etc/os-release").unwrap_or_default();
        let field = |key: &str| {
            os.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or("unknown")
                .trim_matches('"')
                .to_string()
        };
        let os = format!("{} {}", field("ID"), field("VERSION_ID"));
        self.add(
            "Operating system",
            ok_or(h.os.contains(&os), Status::Fail),
            &os,
            Some(format!("Use one of: {}", h.os.join(", "))),
        );
        self.add(
            "Architecture",
            ok_or(env::consts::ARCH == h.arch, Status::Fail),
            env::consts::ARCH,
            Some(format!("Use a host with {} architecture", h.arch)),
        );

        if let Some(version) = wsl() {
            let interop = wsl_interop();
            self.add(
                "WSL",
                ok_or(version == 2 && interop, Status::Warn),
                format!(
                    "WSL{version}, interop {}",
                    if interop { "enabled" } else { "disabled" }
                ),
                Some("The viewer uses the GPU only on WSL2 with interop. Run wsl --set-version <distro> 2 in Windows, and set [interop] enabled=true in /etc/wsl.conf".into()),
            );
        }

        let want = h.recommended_memory_gib;
        let kib = fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        });
        match kib {
            Some(kib) => self.add(
                "Memory",
                ok_or(kib >= want << 20, Status::Warn),
                format!(
                    "{:.1} GiB (recommended >= {want} GiB)",
                    kib as f64 / 1048576.0
                ),
                Some(
                    "Use cargo build -j to reduce parallel jobs if compilation runs out of memory"
                        .into(),
                ),
            ),
            None => self.add("Memory", Status::Warn, "Cannot read /proc/meminfo", None),
        }

        let want = h.min_free_disk_gib;
        let root = self.t.root.to_string_lossy();
        let avail = self
            .t
            .output("df", &["--output=avail", "-B1", &root])
            .and_then(|s| {
                s.lines()
                    .last()
                    .and_then(|l| l.trim().parse::<u64>().ok())
                    .ok_or(format!("Cannot parse df output: {s}"))
            });
        match avail {
            Ok(b) => self.add(
                "Disk",
                ok_or(b >= want << 30, Status::Fail),
                format!("{} GiB available (requires >= {want} GiB)", b >> 30),
                Some("Free space on the disk containing the project".into()),
            ),
            Err(e) => self.add("Disk", Status::Fail, e, None),
        }
        self.apt();
    }

    fn apt(&mut self) {
        let mut want = self.t.m.apt.clone();
        if windows_viewer() {
            let windows = &self.t.m.wsl.apt;
            want.extend(windows.iter().map(|p| (p.clone(), String::new())));
        }
        let mut args = vec!["-W", "-f", "${Package}\t${db:Status-Status}\t${Version}\n"];
        args.extend(want.keys().map(String::as_str));
        // Exit 1 means unknown packages. stdout is still valid.
        let out = match self.t.command(Path::new("dpkg-query")).args(&args).output() {
            Ok(o) if matches!(o.status.code(), Some(0 | 1)) => o,
            Ok(o) => {
                return self.add(
                    "System packages",
                    Status::Fail,
                    format!("dpkg-query {}", o.status),
                    None,
                );
            }
            Err(e) => return self.add("System packages", Status::Fail, e.to_string(), None),
        };
        let installed = parse_dpkg(&String::from_utf8_lossy(&out.stdout));
        let bad: Vec<String> = want
            .iter()
            .filter(|(name, min)| match installed.get(*name) {
                None => true,
                Some(_) if min.is_empty() => false,
                Some(v) => !self.at_least(v, min),
            })
            .map(|(name, _)| name.clone())
            .collect();
        if bad.is_empty() {
            let n = want.len();
            self.add(
                "System packages",
                Status::Ok,
                format!("All {n} packages meet requirements"),
                None,
            );
        } else {
            let list = bad.join(" ");
            self.add(
                "System packages",
                Status::Fail,
                format!("Missing or outdated: {list}"),
                Some(format!("sudo apt update && sudo apt install -y {list}")),
            );
        }
    }

    fn rust(&mut self) {
        let want = env!("CARGO_PKG_RUST_VERSION");
        let rustc = self
            .t
            .cargo_bin("rustc")
            .ok_or("rustc managed by rustup not found".to_string())
            .and_then(|p| self.t.output(p, &["--version"]));
        let good = rustc
            .as_ref()
            .ok()
            .and_then(|v| v.split_whitespace().nth(1))
            .is_some_and(|v| self.at_least(v, want));
        self.add(
            "Rust",
            ok_or(good, Status::Fail),
            format!("{} (requires >= {want})", rustc.unwrap_or_else(|e| e)),
            Some("Install rustup (https://rustup.rs), then run rustup update stable".into()),
        );
        for (name, component) in [("cargo-clippy", "clippy"), ("cargo-fmt", "rustfmt")] {
            let got = self
                .t
                .cargo_bin(name)
                .ok_or(format!("{name} not found"))
                .and_then(|p| self.t.output(p, &["--version"]));
            self.add(
                component,
                ok_or(got.is_ok(), Status::Fail),
                got.unwrap_or_else(|e| e),
                Some(format!("rustup component add {component}")),
            );
        }
    }

    fn esp_idf(&mut self) {
        let idf = self.t.idf();
        let want = &self.t.m.esp_idf;
        if !idf.join(".git").exists() {
            return self.add(
                "ESP-IDF",
                Status::Fail,
                format!("{} does not exist or is not a git clone", idf.display()),
                setup_fix(),
            );
        }
        let dir = idf.to_string_lossy().into_owned();
        // Prevent index writes during status checks.
        let git = |args: &[&str]| {
            let mut all = vec!["--no-optional-locks", "-C", dir.as_str()];
            all.extend(args);
            self.t.output("git", &all)
        };
        let head = git(&["rev-parse", "HEAD"]);
        let dirty = git(&["status", "--porcelain"]);
        let submodules = git(&["submodule", "status", "--recursive"]);

        self.add(
            "ESP-IDF commit",
            ok_or(head.as_ref() == Ok(&want.commit), Status::Fail),
            head.unwrap_or_else(|e| e),
            setup_fix(),
        );
        match dirty {
            Ok(s) => self.add(
                "ESP-IDF working tree",
                ok_or(s.is_empty(), Status::Fail),
                if s.is_empty() {
                    "Clean".into()
                } else {
                    format!("{} paths changed", s.lines().count())
                },
                Some(format!("Inspect with git -C {dir} status")),
            ),
            Err(e) => self.add("ESP-IDF working tree", Status::Fail, e, None),
        }
        match submodules {
            Ok(s) => {
                let bad = bad_submodules(&s);
                self.add(
                    "ESP-IDF submodule",
                    ok_or(bad == 0, Status::Fail),
                    format!("{bad} uninitialized or mismatched submodules"),
                    setup_fix(),
                )
            }
            Err(e) => self.add("ESP-IDF submodule", Status::Fail, e, None),
        }

        let home = self.t.idf_home();
        match idf_tools(&idf, &want.targets) {
            Ok(list) => {
                let missing: Vec<_> = list
                    .iter()
                    .filter(|(name, ver)| !home.join("tools").join(name).join(ver).is_dir())
                    .map(|(name, ver)| format!("{name}@{ver}"))
                    .collect();
                self.add(
                    "ESP-IDF toolchain",
                    ok_or(missing.is_empty(), Status::Fail),
                    if missing.is_empty() {
                        format!("All {} tools present", list.len())
                    } else {
                        format!("Missing {}", missing.join(" "))
                    },
                    setup_fix(),
                )
            }
            Err(e) => self.add("ESP-IDF toolchain", Status::Fail, e, None),
        }
    }

    fn python(&mut self) {
        let want = &self.t.m.python;
        let uv = find_in(user_path(), "uv")
            .ok_or("uv not found".to_string())
            .and_then(|p| self.t.output(p, &["--version"]));
        let good = uv
            .as_ref()
            .ok()
            .and_then(|s| s.split_whitespace().nth(1))
            .is_some_and(|v| self.at_least(v, &want.min_uv));
        self.add(
            "uv",
            ok_or(good, Status::Fail),
            format!("{} (requires >= {})", uv.unwrap_or_else(|e| e), want.min_uv),
            Some("Install or update uv: https://docs.astral.sh/uv/".into()),
        );

        let venv = self.t.venv();
        let got = fs::read_to_string(venv.join("pyvenv.cfg"))
            .map_err(|_| {
                format!(
                    "{} does not exist or is not a virtual environment",
                    venv.display()
                )
            })
            .and_then(|cfg| check_venv(&cfg, &want.version))
            .and_then(|()| {
                let code = "import esptool, idf_component_manager";
                self.t
                    .output(venv.join("bin/python"), &["-I", "-B", "-c", code])
            });
        self.add(
            "ESP-IDF Python",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(|e| e, |_| format!("Managed by uv, Python {}", want.version)),
            setup_fix(),
        );
    }

    fn esp_emu(&mut self) {
        let want = &self.t.m.esp_emu;
        let bin = self.t.esp_emu();
        let fix = setup_fix();
        if !bin.is_file() {
            let evidence = format!("{} does not exist", bin.display());
            return self.add("esp-emu", Status::Fail, evidence, fix);
        }
        if let Err(e) = self.t.verify(&bin, &want.binary_sha256) {
            return self.add("esp-emu", Status::Fail, e, fix);
        }
        let got = self.t.output(&bin, &["--version"]);
        let good = got
            .as_ref()
            .is_ok_and(|v| v.split_whitespace().any(|w| w == want.version));
        self.add(
            "esp-emu",
            ok_or(good, Status::Fail),
            got.unwrap_or_else(|e| e),
            fix,
        );
    }

    fn windows_target(&mut self) {
        if !windows_viewer() {
            return;
        }
        let installed = self
            .t
            .cargo_bin("rustup")
            .ok_or("rustup not found".to_string())
            .and_then(|p| self.t.output(p, &["target", "list", "--installed"]));
        let good = installed
            .as_ref()
            .is_ok_and(|s| s.lines().any(|l| l.trim() == WINDOWS_TARGET));
        self.add(
            "Windows viewer target",
            ok_or(good, Status::Fail),
            match installed {
                Ok(_) if good => format!("{WINDOWS_TARGET} installed"),
                Ok(_) => format!("{WINDOWS_TARGET} missing"),
                Err(e) => e,
            },
            setup_fix(),
        );
    }

    fn isolation(&mut self) {
        let mut shadowed = vec![];
        for name in ["git", "cmake", "ninja", "mosquitto", "idf.py", "esp-emu"] {
            let ours = self.t.which(name);
            if let Some(theirs) = find_in(user_path(), name)
                && ours.as_ref().and_then(|p| p.canonicalize().ok()) != theirs.canonicalize().ok()
            {
                shadowed.push(theirs.display().to_string());
            }
        }
        self.add(
            "PATH shadowing",
            ok_or(shadowed.is_empty(), Status::Warn),
            if shadowed.is_empty() {
                "No other versions in the interactive shell".into()
            } else {
                format!("Interactive shell uses {}", shadowed.join(" "))
            },
            Some("Commands run through coldguard are unaffected. Check versions when running manually".into()),
        );
        let rust = env::var("RUSTUP_TOOLCHAIN").ok();
        let pinned = fs::read_to_string(self.t.root.join("rust-toolchain.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .and_then(|t| Some(t.get("toolchain")?.get("channel")?.as_str()?.to_string()))
            .unwrap_or_default();
        let good = rust.as_ref().is_none_or(|v| v.starts_with(&pinned));
        self.add(
            "RUSTUP_TOOLCHAIN",
            ok_or(good, Status::Warn),
            rust.unwrap_or_else(|| "Not set".into()),
            Some(format!("Overrides the pinned {pinned}. Unset it, or reopen the shell so mise reads mise.toml")),
        );
        let vars: Vec<_> = ["IDF_PATH", "IDF_TOOLS_PATH", "IDF_PYTHON_ENV_PATH"]
            .into_iter()
            .filter(|k| env::var_os(k).is_some())
            .collect();
        self.add(
            "IDF environment variables",
            ok_or(vars.is_empty(), Status::Warn),
            if vars.is_empty() {
                "Not set".into()
            } else {
                format!("Set: {}", vars.join(" "))
            },
            Some("Ignored by coldguard. Unset before running manually".into()),
        );
    }

    fn runtime(&mut self) {
        let port = self.t.m.mqtt.port;
        let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, port));
        self.add(
            "MQTT port",
            ok_or(bound.is_ok(), Status::Warn),
            match &bound {
                Ok(_) => format!("{port} available"),
                Err(e) => format!("{port}: {e}"),
            },
            Some(format!(
                "Check the listener with ss -ltnp 'sport = :{port}'"
            )),
        );
        drop(bound);

        let pids: Vec<String> = fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "esp-emu")
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        self.add(
            "esp-emu processes",
            ok_or(pids.is_empty(), Status::Warn),
            if pids.is_empty() {
                "No running processes".into()
            } else {
                format!("Running PIDs: {}", pids.join(" "))
            },
            Some("Check for stale processes and terminate them manually".into()),
        );

        let got = smoke::mqtt_roundtrip(self.t);
        self.add(
            "MQTT round trip",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(
                |e| e,
                |_| "Send and receive succeeded through a temporary broker".into(),
            ),
            Some("Fix system packages first. If it still fails, run mosquitto -v manually".into()),
        );
    }

    fn project(&mut self) {
        let got = self
            .t
            .cargo_bin("cargo")
            .ok_or("cargo not found".to_string())
            .and_then(|p| {
                self.t.output(
                    p,
                    &[
                        "metadata",
                        "--locked",
                        "--offline",
                        "--filter-platform",
                        "x86_64-unknown-linux-gnu",
                        "--format-version",
                        "1",
                    ],
                )
            });
        self.add(
            "Cargo.lock",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(|e| e, |_| "Matches Cargo.toml".into()),
            Some("Run cargo fetch --locked. For intentional dependency changes, run cargo check and commit Cargo.lock".into()),
        );
        let got = smoke::check_build(self.t);
        self.add(
            "Firmware and IDF version",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(|e| e, |_| self.t.m.esp_idf.version.clone()),
            setup_fix(),
        );
    }

    fn end_to_end(&mut self) {
        let boot = smoke::boot(self.t);
        for (id, got) in [
            ("S3 firmware boot", boot.boot),
            ("cosim handshake", boot.cosim),
        ] {
            let status = ok_or(got.is_ok(), Status::Fail);
            self.add(id, status, got.unwrap_or_else(|e| e), setup_fix());
        }
        let got = smoke::firmware_mqtt(self.t);
        self.add(
            "Firmware MQTT round trip",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(
                |e| e,
                |_| "Wi-Fi connection, send, and receive succeeded".into(),
            ),
            Some(
                "Fix system packages and the MQTT round trip first, then run cargo run -- setup"
                    .into(),
            ),
        );
        match boot.speed {
            Ok(x) => self.add(
                "Speed ratio",
                ok_or(x >= 1.0, Status::Warn),
                format!("{x:.2}x (single idle node, 1 ms quantum, not an S4 result)"),
                Some("Stop other workloads and test again".into()),
            ),
            Err(e) => self.add("Speed ratio", Status::Fail, e, setup_fix()),
        }
    }
}

fn setup_fix() -> Option<String> {
    Some("cargo run -- setup".into())
}

fn ok_or(good: bool, otherwise: Status) -> Status {
    if good { Status::Ok } else { otherwise }
}

fn parse_dpkg(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (name, status, version) = (f.next()?, f.next()?, f.next()?);
            (status == "installed").then(|| (name.into(), version.into()))
        })
        .collect()
}

/// Count uninitialized, mismatched, or conflicted submodules.
fn bad_submodules(out: &str) -> usize {
    out.lines()
        .filter(|l| l.starts_with(['-', '+', 'U']))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_ignores_packages_not_installed() {
        let m = parse_dpkg(
            "git\tinstalled\t1:2.43\npython3-venv\tnot-installed\t\nx\tconfig-files\t1\n",
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m["git"], "1:2.43");
    }

    #[test]
    fn submodule_states() {
        assert_eq!(bad_submodules(" aa a (v1)\n-bb b\n+cc c (v2)\nUdd d\n"), 3);
        // Trimming the first line must not mark it as bad.
        assert_eq!(bad_submodules("aa a (v1)\n bb b (v1)"), 0);
    }
}
