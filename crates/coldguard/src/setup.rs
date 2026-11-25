//! Idempotent installation into .tools.

use crate::toolchain::{Toolchain, check_venv, find_in, user_path};
use std::{error::Error, fs, path::Path, process::Command};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

pub fn run(t: &Toolchain) -> Result {
    for name in ["git", "curl", "tar", "sha256sum", "cmake", "ninja"] {
        if t.which(name).is_none() {
            return Err(format!(
                "{name} not found. Install system packages as directed by doctor first"
            )
            .into());
        }
    }
    fs::create_dir_all(t.tools())?;
    step("ESP-IDF", esp_idf(t))?;
    step("ESP-IDF Python", python(t))?;
    step("ESP-IDF toolchain", idf_tools(t))?;
    step("esp-emu", esp_emu(t))?;
    step("Cargo dependencies", cargo(t))?;
    step("Smoke firmware", smoke(t))?;
    step("Node firmware", node(t))
}

fn step(name: &str, result: Result) -> Result {
    result.map_err(|e| format!("{name}: {e}"))?;
    eprintln!("✓ {name}");
    Ok(())
}

fn check(c: &mut Command) -> Result {
    let status = c.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{}: {status}", c.get_program().to_string_lossy()).into())
    }
}

fn esp_idf(t: &Toolchain) -> Result {
    let want = &t.m.esp_idf;
    let dir = t.idf();
    let git = || {
        let mut c = t.plain("git");
        c.arg("-C").arg(&dir);
        c
    };
    if !dir.join(".git").exists() {
        // git refuses to clone into a non-empty directory.
        check(
            t.plain("git")
                .args(["-c", "advice.detachedHead=false", "clone", "--depth", "1"])
                .args(["--branch", &want.version, &want.url])
                .arg(&dir),
        )?;
    }
    let idf = dir.to_string_lossy();
    let head = || t.output("git", &["-C", &idf, "rev-parse", "HEAD"]);
    if head()? != want.commit {
        // Submodule drift is repaired below.
        let args = ["-C", &idf, "status", "--porcelain", "--ignore-submodules"];
        if !t.output("git", &args)?.is_empty() {
            return Err(
                format!("{idf} has uncommitted changes. Refusing to switch versions").into(),
            );
        }
        let tag = format!("refs/tags/{0}:refs/tags/{0}", want.version);
        check(git().args(["fetch", "--depth", "1", &want.url, &tag]))?;
        check(git().args(["checkout", "--detach", &want.commit]))?;
    }
    check(git().args(["submodule", "sync", "--recursive", "--quiet"]))?;
    check(
        git()
            .args(["submodule", "update", "--init", "--recursive"])
            .args(["--depth", "1", "--jobs", "8"]),
    )
}

fn python(t: &Toolchain) -> Result {
    let want = &t.m.python;
    let uv = find_in(user_path(), "uv").ok_or("uv not found (https://docs.astral.sh/uv/)")?;
    let uv = || {
        let mut c = t.plain(&uv);
        c.env("UV_PYTHON_INSTALL_DIR", t.tools().join("python"))
            .current_dir(t.tools());
        c
    };
    let venv = t.venv();
    let current = fs::read_to_string(venv.join("pyvenv.cfg"))
        .map_err(|e| e.to_string())
        .and_then(|cfg| check_venv(&cfg, &want.version));
    if current.is_err() || !venv.join("bin/python").exists() {
        let args = ["venv", "--clear", "--managed-python", "--python"];
        check(uv().args(args).arg(&want.version).arg(&venv))?;
    }

    let minor =
        t.m.esp_idf
            .version
            .rsplit_once('.')
            .ok_or("Invalid ESP-IDF version format")?
            .0;
    let name = format!("espidf.constraints.{minor}.txt");
    let constraints = t.idf_home().join(&name);
    if !constraints.is_file() {
        fs::create_dir_all(t.idf_home())?;
        download(
            t,
            &format!("https://dl.espressif.com/dl/esp-idf/{name}"),
            &constraints,
        )?;
    }
    let requirements = t.idf().join("tools/requirements/requirements.core.txt");
    check(
        uv().args(["pip", "install", "--quiet", "--python"])
            .arg(venv.join("bin/python"))
            .arg("-r")
            .arg(requirements)
            .arg("-c")
            .arg(constraints),
    )
}

fn idf_tools(t: &Toolchain) -> Result {
    let targets = format!("--targets={}", t.m.esp_idf.targets.join(","));
    check(
        t.idf_tools_command()
            .args(["install", &targets, "required"]),
    )
}

fn esp_emu(t: &Toolchain) -> Result {
    let want = &t.m.esp_emu;
    let bin = t.esp_emu();
    if bin.is_file() && verify(t, &bin, &want.binary_sha256).is_ok() {
        return Ok(());
    }
    let tmp = t.tools().join("esp-emu.tmp");
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let archive = tmp.join("esp-emu.tar.gz");
    download(t, &want.url, &archive)?;
    verify(t, &archive, &want.archive_sha256)?;
    check(
        t.plain("tar")
            .args(["--strip-components=1", "--no-same-owner", "-xzf"])
            .arg(&archive)
            .arg("-C")
            .arg(&tmp),
    )?;
    verify(t, &tmp.join("esp-emu"), &want.binary_sha256)?;
    fs::create_dir_all(bin.parent().ok_or("Invalid esp-emu path")?)?;
    fs::rename(tmp.join("esp-emu"), &bin)?;
    fs::remove_dir_all(&tmp)?;
    Ok(())
}

fn cargo(t: &Toolchain) -> Result {
    let cargo = t.cargo_bin("cargo").ok_or("cargo not found")?;
    check(
        t.plain(cargo)
            .args(["fetch", "--locked"])
            .current_dir(&t.root),
    )
}

fn smoke(t: &Toolchain) -> Result {
    firmware(t, &t.smoke_source(), &t.smoke_image())
}

pub fn node(t: &Toolchain) -> Result {
    firmware(t, &t.node_source(), &t.node_image())
}

fn firmware(t: &Toolchain, source: &Path, image: &Path) -> Result {
    let build = image.parent().ok_or("image has no directory")?;
    // An existing sdkconfig hides changes to sdkconfig.defaults.
    let _ = fs::remove_file(build.join("sdkconfig"));
    let sdkconfig = format!("SDKCONFIG={}", build.join("sdkconfig").display());
    check(
        t.idf_command("idf.py")?
            .arg("-C")
            .arg(source)
            .arg("-B")
            .arg(build)
            .args(["-D", &sdkconfig, "-D", "IDF_TARGET=esp32s3"])
            .args(["build", "merge-bin", "-o"])
            .arg(image),
    )
}

/// Publish only complete files.
fn download(t: &Toolchain, url: &str, to: &Path) -> Result {
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    check(
        t.plain("curl")
            .args(["--proto", "=https", "--proto-redir", "=https"])
            .args(["-fsSL", "--retry", "3", "--connect-timeout", "30", "-o"])
            .arg(&part)
            .arg(url),
    )?;
    fs::rename(&part, to)?;
    Ok(())
}

fn verify(t: &Toolchain, file: &Path, want: &str) -> Result {
    let got = t.output("sha256sum", &[&file.to_string_lossy()])?;
    if got.split_whitespace().next() == Some(want) {
        Ok(())
    } else {
        Err(format!("SHA-256 mismatch: {got}").into())
    }
}
