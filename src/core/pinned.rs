//! Versions of vfox/uv/caddy this build of `stack` is actually tested against —
//! checked by `doctor` (warns, doesn't hard-block, on a mismatch with what's
//! installed) and used by `doctor --fix` to fetch an exact, reproducible version via
//! `platform::install_pinned` rather than "whatever's currently latest." See
//! PLAN.md section 20 for why this replaces going through winget/brew/apt directly.
//!
//! `uv`'s pinned version here (0.11.7) is deliberately *not* the newest available
//! (winget currently offers 0.11.30) — it's the version this whole project has
//! actually been built and tested against this session. That gap is exactly the
//! scenario pinning exists to guard against: "install latest" would silently hand a
//! fresh machine a different uv than the one `stack` was verified with.
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
