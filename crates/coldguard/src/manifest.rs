use serde::Deserialize;
use std::{collections::BTreeMap, error::Error, fs, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub host: Host,
    pub env: Env,
    pub python: Python,
    pub esp_idf: EspIdf,
    pub esp_emu: EspEmu,
    pub dozen: Dozen,
    pub mqtt: Mqtt,
    pub apt: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub os: Vec<String>,
    pub arch: String,
    pub min_free_disk_gib: u64,
    pub recommended_memory_gib: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    pub path: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Python {
    pub version: String,
    pub min_uv: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EspIdf {
    pub version: String,
    pub url: String,
    pub commit: String,
    pub targets: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EspEmu {
    pub version: String,
    pub url: String,
    pub archive_sha256: String,
    pub binary_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dozen {
    pub version: String,
    pub url: String,
    pub archive_sha256: String,
    pub python: Vec<String>,
    pub apt: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mqtt {
    pub port: u16,
}

pub fn load(root: &Path) -> Result<Manifest, Box<dyn Error>> {
    Ok(toml::from_str(&fs::read_to_string(
        root.join("toolchain.toml"),
    )?)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn repo_manifest_is_valid() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let m = super::load(&root).unwrap();
        let hex = |s: &str, n| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        assert!(hex(&m.esp_idf.commit, 40));
        assert!(hex(&m.esp_emu.archive_sha256, 64));
        assert!(hex(&m.esp_emu.binary_sha256, 64));
        assert!(m.esp_idf.url.starts_with("https://"));
        assert!(m.esp_emu.url.contains(&m.esp_emu.version));
        assert!(hex(&m.dozen.archive_sha256, 64));
        assert!(m.dozen.url.contains(&m.dozen.version));
    }
}
