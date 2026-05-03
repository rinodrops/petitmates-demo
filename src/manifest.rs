use std::collections::HashMap;
use std::path::Path;

#[derive(serde::Deserialize)]
pub struct Manifest {
    pub canonical_width: f64,
    #[serde(default)]
    pub sprites: HashMap<String, SpriteInfo>,
}

#[derive(serde::Deserialize)]
pub struct SpriteInfo {
    #[allow(dead_code)]
    pub attachment: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

pub fn load(char_dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(char_dir.join("manifest.toml")).ok()?;
    toml::from_str(&text).ok()
}
