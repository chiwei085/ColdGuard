use crate::toolchain::{Toolchain, find_in, user_path};
use std::{error::Error, process::Stdio};

pub fn run(t: &Toolchain) -> Result<(), Box<dyn Error>> {
    let blender = t.which("blender").ok_or("Blender 4.0 not found")?;
    let uv = find_in(user_path(), "uv").ok_or("uv not found")?;
    let project = t.root.join("crates/viewer/blender");
    let output = t
        .plain(uv)
        .args(["run", "--locked", "--project"])
        .arg(&project)
        .args([
            "python",
            "-c",
            "import sysconfig; print(sysconfig.get_path('platlib'))",
        ])
        .current_dir(&t.root)
        .stderr(Stdio::inherit())
        .output()?;
    if !output.status.success() {
        return Err(format!("Blender dependencies: {}", output.status).into());
    }
    let packages = String::from_utf8(output.stdout)?;
    let status = t
        .plain(blender)
        .env("PYTHONPATH", packages.trim())
        .args([
            "-b",
            "--factory-startup",
            "--python-use-system-env",
            "--python-exit-code",
            "1",
            "-P",
        ])
        .arg(project.join("clinic.py"))
        .current_dir(&t.root)
        .status()?;
    if !status.success() {
        return Err(format!("Blender asset build: {status}").into());
    }
    Ok(())
}
