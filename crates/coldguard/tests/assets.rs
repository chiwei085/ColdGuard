use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn assets_passes_the_environment_and_reports_failures() {
    let root = std::env::temp_dir().join(format!("coldguard assets-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let mut manifest: toml::Value =
        toml::from_str(include_str!("../../../toolchain.toml")).unwrap();
    manifest["env"]["path"] =
        toml::Value::Array(vec![toml::Value::String(root.to_str().unwrap().to_owned())]);
    fs::write(
        root.join("toolchain.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    let script = |name: &str, body: &str| {
        let path = root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_coldguard"))
            .arg("assets")
            .current_dir(&root)
            .env("PATH", &root)
            .env("PYTHONPATH", "/unrelated/packages")
            .output()
            .unwrap()
    };
    script("uv", "printf '/packages with spaces\\n'");
    script(
        "blender",
        r#"[ "$PYTHONPATH" = '/packages with spaces' ] || exit 31
[ "$*" = "-b --factory-startup --python-use-system-env --python-exit-code 1 -P $PWD/crates/viewer/blender/clinic.py" ] || exit 32
[ "$#" = 7 ] || exit 33
printf invoked > blender-ran"#,
    );
    let output = run();
    assert!(output.status.success(), "{output:?}");
    assert!(root.join("blender-ran").is_file());

    script("blender", "exit 23");
    let output = run();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Blender asset build: exit status: 23")
    );

    fs::remove_file(root.join("blender-ran")).unwrap();
    script("uv", "exit 17");
    script("blender", "printf invoked > blender-ran");
    let output = run();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Blender dependencies: exit status: 17")
    );
    assert!(!root.join("blender-ran").exists());
    fs::remove_dir_all(root).unwrap();
}
