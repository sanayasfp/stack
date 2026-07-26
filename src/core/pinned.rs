pub const VFOX_VERSION: &str = "1.0.11";
pub const UV_VERSION: &str = "0.11.7";
pub const CADDY_VERSION: &str = "2.11.4";

pub fn pinned_version(tool: &str) -> Option<&'static str> {
    match tool {
        "vfox" => Some(VFOX_VERSION),
        "uv" => Some(UV_VERSION),
        "caddy" => Some(CADDY_VERSION),
        _ => None,
    }
}
