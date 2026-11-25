//! Read-only toolchain checks with evidence and fixes.

use crate::manifest::Manifest;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs,
    net::{Ipv4Addr, TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Todo,
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
    root: &'a Path,
    m: &'a Manifest,
    layer: &'static str,
    checks: Vec<Check>,
}

pub fn run(root: &Path, m: &Manifest) -> Vec<Check> {
    let mut d = Doctor {
        root,
        m,
        layer: "主機",
        checks: vec![],
    };
    d.host();
    d.layer = "工具";
    d.rust();
    d.python();
    d.esp_idf();
    d.esp_emu();
    d.layer = "隔離";
    d.isolation();
    d.layer = "執行";
    d.runtime();
    d.layer = "專案";
    d.project();
    d.layer = "端到端";
    for id in ["S3 韌體開機", "cosim 交握", "韌體 MQTT 往返", "速度比"] {
        d.add(id, Status::Todo, "未實作", None);
    }
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
            Status::Todo => "·",
        };
        println!("  {mark} {}：{}", c.id, c.evidence);
        if let Some(fix) = &c.fix {
            println!("      → {fix}");
        }
    }
    let n = |s| checks.iter().filter(|c| c.status == s).count();
    println!(
        "\n{} 通過、{} 警告、{} 失敗、{} 未實作",
        n(Status::Ok),
        n(Status::Warn),
        n(Status::Fail),
        n(Status::Todo)
    );
}

impl Doctor<'_> {
    fn add(
        &mut self,
        id: impl Into<String>,
        status: Status,
        evidence: impl Into<String>,
        fix: Option<String>,
    ) {
        let fix = fix.filter(|_| matches!(status, Status::Warn | Status::Fail));
        self.checks.push(Check {
            layer: self.layer,
            id: id.into(),
            status,
            evidence: evidence.into(),
            fix,
        });
    }

    fn tools(&self) -> PathBuf {
        self.root.join(".tools")
    }

    /// Run with an allowlisted environment and a 60-second timeout.
    fn command(&self, program: &Path) -> Command {
        let mut c = Command::new("/usr/bin/timeout");
        c.args(["--kill-after=5", "60"])
            .arg(program)
            .env_clear()
            .env("PATH", self.m.env.path.join(":"))
            .env("LC_ALL", "C.UTF-8")
            // Keep checks read-only.
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("ESP_EMU_DISABLE_AUTOUPDATES", "1")
            .current_dir(self.root)
            .stdin(Stdio::null());
        for key in ["HOME", "CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(v) = env::var_os(key) {
                c.env(key, v);
            }
        }
        c
    }

    fn output(&self, program: impl AsRef<Path>, args: &[&str]) -> Result<String, String> {
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
            let detail = err.trim().lines().last().unwrap_or("無錯誤輸出");
            Err(format!(
                "{}：{}（{}）",
                program.display(),
                detail,
                out.status
            ))
        }
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        find_in(self.m.env.path.iter().map(PathBuf::from), name)
    }

    fn host(&mut self) {
        let h = &self.m.host;
        let os = fs::read_to_string("/etc/os-release").unwrap_or_default();
        let field = |key: &str| {
            os.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or("未知")
                .trim_matches('"')
                .to_string()
        };
        let (id, version) = (field("ID"), field("VERSION_ID"));
        let status = ok_or(id == h.os_id && version == h.os_version, Status::Fail);
        self.add(
            "作業系統",
            status,
            format!("{id} {version}"),
            Some(format!("使用 {} {}", h.os_id, h.os_version)),
        );
        self.add(
            "架構",
            ok_or(env::consts::ARCH == h.arch, Status::Fail),
            env::consts::ARCH,
            Some(format!("使用 {} 主機", h.arch)),
        );

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
                "記憶體",
                ok_or(kib >= want << 20, Status::Warn),
                format!("{:.1} GiB（建議 ≥ {want} GiB）", kib as f64 / 1048576.0),
                Some("編譯記憶體不足時以 cargo build -j 降低並行數".into()),
            ),
            None => self.add("記憶體", Status::Warn, "無法讀取 /proc/meminfo", None),
        }

        let want = h.min_free_disk_gib;
        let root = self.root.to_string_lossy();
        let avail = self
            .output("df", &["--output=avail", "-B1", &root])
            .and_then(|s| {
                s.lines()
                    .last()
                    .and_then(|l| l.trim().parse::<u64>().ok())
                    .ok_or(format!("無法解析 df 輸出：{s}"))
            });
        match avail {
            Ok(b) => self.add(
                "磁碟",
                ok_or(b >= want << 30, Status::Fail),
                format!("可用 {} GiB（需 ≥ {want} GiB）", b >> 30),
                Some("釋出專案所在磁碟的空間".into()),
            ),
            Err(e) => self.add("磁碟", Status::Fail, e, None),
        }
        self.apt();
    }

    fn apt(&mut self) {
        let mut args = vec!["-W", "-f", "${Package}\t${db:Status-Status}\t${Version}\n"];
        args.extend(self.m.apt.keys().map(String::as_str));
        // Exit 1 means unknown packages. stdout is still valid.
        let out = match self.command(Path::new("dpkg-query")).args(&args).output() {
            Ok(o) if matches!(o.status.code(), Some(0 | 1)) => o,
            Ok(o) => {
                return self.add(
                    "系統套件",
                    Status::Fail,
                    format!("dpkg-query {}", o.status),
                    None,
                );
            }
            Err(e) => return self.add("系統套件", Status::Fail, e.to_string(), None),
        };
        let installed = parse_dpkg(&String::from_utf8_lossy(&out.stdout));
        let bad: Vec<String> = self
            .m
            .apt
            .iter()
            .filter(|(name, min)| match installed.get(*name) {
                None => true,
                Some(_) if min.is_empty() => false,
                Some(v) => self
                    .output("dpkg", &["--compare-versions", v, "ge", min])
                    .is_err(),
            })
            .map(|(name, _)| name.clone())
            .collect();
        if bad.is_empty() {
            let n = self.m.apt.len();
            self.add("系統套件", Status::Ok, format!("{n} 個皆符合"), None);
        } else {
            let list = bad.join(" ");
            self.add(
                "系統套件",
                Status::Fail,
                format!("缺少或版本過舊：{list}"),
                Some(format!("sudo apt update && sudo apt install -y {list}")),
            );
        }
    }

    fn cargo_bin(&self, name: &str) -> Option<PathBuf> {
        let home = env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".cargo")))?;
        find_in([home.join("bin")].into_iter(), name)
    }

    fn rust(&mut self) {
        let want = env!("CARGO_PKG_RUST_VERSION");
        let rustc = self
            .cargo_bin("rustc")
            .ok_or("找不到 rustup 的 rustc".to_string())
            .and_then(|p| self.output(p, &["--version"]));
        let good = rustc
            .as_ref()
            .ok()
            .and_then(|v| v.split_whitespace().nth(1))
            .is_some_and(|v| {
                self.output("dpkg", &["--compare-versions", v, "ge", want])
                    .is_ok()
            });
        self.add(
            "Rust",
            ok_or(good, Status::Fail),
            format!("{}（需 ≥ {want}）", rustc.unwrap_or_else(|e| e)),
            Some("安裝 rustup（https://rustup.rs）後執行 rustup update stable".into()),
        );
        for (name, component) in [("cargo-clippy", "clippy"), ("cargo-fmt", "rustfmt")] {
            let got = self
                .cargo_bin(name)
                .ok_or(format!("找不到 {name}"))
                .and_then(|p| self.output(p, &["--version"]));
            self.add(
                component,
                ok_or(got.is_ok(), Status::Fail),
                got.unwrap_or_else(|e| e),
                Some(format!("rustup component add {component}")),
            );
        }
    }

    fn esp_idf(&mut self) {
        let idf = self.tools().join("esp-idf");
        let want = &self.m.esp_idf;
        if !idf.join(".git").exists() {
            return self.add(
                "ESP-IDF",
                Status::Fail,
                format!("{} 不存在或不是 git clone", idf.display()),
                Some(format!(
                    "安裝 {}（{}，setup 尚未實作）",
                    want.version, want.url
                )),
            );
        }
        let dir = idf.to_string_lossy().into_owned();
        // Prevent index writes during status checks.
        let git = |args: &[&str]| {
            let mut all = vec!["--no-optional-locks", "-C", dir.as_str()];
            all.extend(args);
            self.output("git", &all)
        };
        let head = git(&["rev-parse", "HEAD"]);
        let dirty = git(&["status", "--porcelain"]);
        let submodules = git(&["submodule", "status", "--recursive"]);

        self.add(
            "ESP-IDF commit",
            ok_or(head.as_ref() == Ok(&want.commit), Status::Fail),
            head.unwrap_or_else(|e| e),
            Some(format!("切換到 {}（{}）", want.version, want.commit)),
        );
        match dirty {
            Ok(s) => self.add(
                "ESP-IDF 工作樹",
                ok_or(s.is_empty(), Status::Fail),
                if s.is_empty() {
                    "未修改".into()
                } else {
                    format!("{} 個路徑被改動", s.lines().count())
                },
                Some(format!("檢視：git -C {dir} status")),
            ),
            Err(e) => self.add("ESP-IDF 工作樹", Status::Fail, e, None),
        }
        match submodules {
            Ok(s) => {
                let bad = bad_submodules(&s);
                self.add(
                    "ESP-IDF submodule",
                    ok_or(bad == 0, Status::Fail),
                    format!("{bad} 個未初始化或不符"),
                    Some(format!(
                        "git -C {dir} submodule update --init --recursive --depth 1"
                    )),
                )
            }
            Err(e) => self.add("ESP-IDF submodule", Status::Fail, e, None),
        }

        let home = self.tools().join("espressif");
        match idf_tools(&idf, &want.targets) {
            Ok(list) => {
                let missing: Vec<_> = list
                    .iter()
                    .filter(|(name, ver)| !home.join("tools").join(name).join(ver).is_dir())
                    .map(|(name, ver)| format!("{name}@{ver}"))
                    .collect();
                self.add(
                    "ESP-IDF 工具鏈",
                    ok_or(missing.is_empty(), Status::Fail),
                    if missing.is_empty() {
                        format!("{} 個皆在", list.len())
                    } else {
                        format!("缺少 {}", missing.join(" "))
                    },
                    Some("安裝目標晶片工具鏈（setup 尚未實作）".into()),
                )
            }
            Err(e) => self.add("ESP-IDF 工具鏈", Status::Fail, e, None),
        }
    }

    fn python(&mut self) {
        let want = &self.m.python;
        let user = env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
        let uv = find_in(user.into_iter(), "uv")
            .ok_or("找不到 uv".to_string())
            .and_then(|p| self.output(p, &["--version"]));
        let good = uv
            .as_ref()
            .ok()
            .and_then(|s| s.split_whitespace().nth(1))
            .is_some_and(|v| {
                self.output("dpkg", &["--compare-versions", v, "ge", &want.min_uv])
                    .is_ok()
            });
        self.add(
            "uv",
            ok_or(good, Status::Fail),
            format!("{}（需 ≥ {}）", uv.unwrap_or_else(|e| e), want.min_uv),
            Some("安裝或更新 uv：https://docs.astral.sh/uv/".into()),
        );

        let venv = self.tools().join("idf-python");
        let got = fs::read_to_string(venv.join("pyvenv.cfg"))
            .map_err(|_| format!("{} 不存在或不是虛擬環境", venv.display()))
            .and_then(|cfg| check_venv(&cfg, &want.version))
            .and_then(|()| {
                let code = "import esptool, idf_component_manager";
                self.output(venv.join("bin/python"), &["-I", "-B", "-c", code])
            });
        self.add(
            "ESP-IDF Python",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(|e| e, |_| format!("uv 管理，Python {}", want.version)),
            Some(format!(
                "以 uv 建立 Python {} 環境（setup 尚未實作）",
                want.version
            )),
        );
    }

    fn esp_emu(&mut self) {
        let want = &self.m.esp_emu;
        let bin = self
            .tools()
            .join("esp-emu")
            .join(&want.version)
            .join("esp-emu");
        let fix = Some(format!(
            "安裝 {}，壓縮檔 SHA-256 {}（setup 尚未實作）",
            want.url, want.archive_sha256
        ));
        if !bin.is_file() {
            let evidence = format!("{} 不存在", bin.display());
            return self.add("esp-emu", Status::Fail, evidence, fix);
        }
        let sum = self.output("sha256sum", &[&bin.to_string_lossy()]);
        let matches = sum
            .as_ref()
            .is_ok_and(|s| s.split_whitespace().next() == Some(&want.binary_sha256));
        if !matches {
            let evidence = sum.map_or_else(|e| e, |s| format!("SHA-256 不符：{s}"));
            return self.add("esp-emu", Status::Fail, evidence, fix);
        }
        let got = self.output(&bin, &["--version"]);
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

    fn isolation(&mut self) {
        let user: Vec<PathBuf> =
            env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect();
        let mut shadowed = vec![];
        for name in ["git", "cmake", "ninja", "mosquitto", "idf.py", "esp-emu"] {
            let ours = self.which(name);
            if let Some(theirs) = find_in(user.iter().cloned(), name)
                && ours.as_ref().and_then(|p| p.canonicalize().ok()) != theirs.canonicalize().ok()
            {
                shadowed.push(theirs.display().to_string());
            }
        }
        self.add(
            "PATH 遮蔽",
            ok_or(shadowed.is_empty(), Status::Warn),
            if shadowed.is_empty() {
                "互動 shell 沒有其他版本".into()
            } else {
                format!("互動 shell 會用到 {}", shadowed.join(" "))
            },
            Some("透過 coldguard 執行不受影響；手動執行請留意".into()),
        );
        let vars: Vec<_> = ["IDF_PATH", "IDF_TOOLS_PATH", "IDF_PYTHON_ENV_PATH"]
            .into_iter()
            .filter(|k| env::var_os(k).is_some())
            .collect();
        self.add(
            "IDF 環境變數",
            ok_or(vars.is_empty(), Status::Warn),
            if vars.is_empty() {
                "未設定".into()
            } else {
                format!("已設定 {}", vars.join(" "))
            },
            Some("coldguard 會忽略；手動執行前請 unset".into()),
        );
    }

    fn runtime(&mut self) {
        let port = self.m.mqtt.port;
        let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, port));
        self.add(
            "MQTT port",
            ok_or(bound.is_ok(), Status::Warn),
            match &bound {
                Ok(_) => format!("{port} 可用"),
                Err(e) => format!("{port}：{e}"),
            },
            Some(format!("查看佔用者：ss -ltnp 'sport = :{port}'")),
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
            "esp-emu 行程",
            ok_or(pids.is_empty(), Status::Warn),
            if pids.is_empty() {
                "無執行中的行程".into()
            } else {
                format!("執行中 PID {}", pids.join(" "))
            },
            Some("確認是否為殘留行程，再自行結束".into()),
        );

        let got = self.mqtt_roundtrip();
        self.add(
            "MQTT 往返",
            ok_or(got.is_ok(), Status::Fail),
            got.map_or_else(|e| e, |_| "暫時 broker 收發成功".into()),
            Some("先修正系統套件；仍失敗則手動執行 mosquitto -v".into()),
        );
    }

    /// Test retained messages with a temporary broker.
    fn mqtt_roundtrip(&self) -> Result<(), String> {
        let need = |n: &str| self.which(n).ok_or(format!("找不到 {n}"));
        let (broker, publ, subs) = (
            need("mosquitto")?,
            need("mosquitto_pub")?,
            need("mosquitto_sub")?,
        );
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|l| l.local_addr())
            .map_err(|e| e.to_string())?
            .port();
        let p = port.to_string();
        let mut child = KillOnDrop(
            self.command(&broker)
                .args(["-p", &p])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?,
        );
        for _ in 0..50 {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
                break;
            }
            if let Ok(Some(s)) = child.0.try_wait() {
                return Err(format!("mosquitto 啟動失敗（{s}）"));
            }
            sleep(Duration::from_millis(100));
        }
        let base = ["-h", "127.0.0.1", "-p", &p, "-t", "coldguard/doctor"];
        self.output(
            publ,
            &[&base[..], &["-r", "-q", "1", "-m", "ping"]].concat(),
        )?;
        let got = self.output(subs, &[&base[..], &["-C", "1", "-W", "5"]].concat())?;
        if got == "ping" {
            Ok(())
        } else {
            Err(format!("收到非預期內容：{got}"))
        }
    }

    fn project(&mut self) {
        let got = self
            .cargo_bin("cargo")
            .ok_or("找不到 cargo".to_string())
            .and_then(|p| {
                self.output(
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
            got.map_or_else(|e| e, |_| "與 Cargo.toml 一致".into()),
            Some("cargo fetch --locked；若有意變更依賴則執行 cargo check 並提交 Cargo.lock".into()),
        );
        self.add("韌體與 IDF 版本", Status::Todo, "未實作", None);
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // timeout forwards SIGTERM to the broker. SIGKILL would orphan it.
        let _ = Command::new("/usr/bin/kill")
            .arg(self.0.id().to_string())
            .status();
        let _ = self.0.wait();
    }
}

fn ok_or(good: bool, otherwise: Status) -> Status {
    if good { Status::Ok } else { otherwise }
}

fn find_in(mut dirs: impl Iterator<Item = PathBuf>, name: &str) -> Option<PathBuf> {
    dirs.find_map(|d| {
        let p = d.join(name);
        let meta = p.metadata().ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(p)
    })
}

/// Require a uv environment with the exact Python version.
fn check_venv(cfg: &str, version: &str) -> Result<(), String> {
    let get = |key: &str| {
        cfg.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim())
    };
    if get("uv").is_none() {
        return Err("虛擬環境不是由 uv 建立".into());
    }
    match get("version_info") {
        Some(v) if v == version => Ok(()),
        v => Err(format!("Python {}，需 {version}", v.unwrap_or("版本未知"))),
    }
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

/// List required tools and recommended versions for the targets.
fn idf_tools(idf: &Path, targets: &[String]) -> Result<Vec<(String, String)>, String> {
    let text = fs::read_to_string(idf.join("tools/tools.json")).map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tools = json["tools"].as_array().ok_or("tools.json 缺少 tools")?;
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
        let name = t["name"].as_str().ok_or("tools.json 工具缺少 name")?;
        let version = t["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|v| v["status"] == "recommended")
            .and_then(|v| v["name"].as_str())
            .ok_or(format!("tools.json：{name} 沒有建議版本"))?;
        list.push((name.into(), version.into()));
    }
    if list.is_empty() {
        return Err("tools.json 沒有符合目標晶片的工具".into());
    }
    Ok(list)
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
    fn venv_must_come_from_uv_with_exact_version() {
        assert!(check_venv("uv = 0.8.4\nversion_info = 3.13.5\n", "3.13.5").is_ok());
        assert!(check_venv("uv = 0.8.4\nversion_info = 3.13.50\n", "3.13.5").is_err());
        assert!(check_venv("version_info = 3.13.5\n", "3.13.5").is_err());
    }

    #[test]
    fn submodule_states() {
        assert_eq!(bad_submodules(" aa a (v1)\n-bb b\n+cc c (v2)\nUdd d\n"), 3);
        // Trimming the first line must not mark it as bad.
        assert_eq!(bad_submodules("aa a (v1)\n bb b (v1)"), 0);
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
