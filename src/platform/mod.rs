//! One uniform call site (`platform::detach`, `platform::kill_tree`, ...) for
//! everything that's genuinely OS-*behavior*-specific — exactly one implementation
//! is compiled in per target via `#[cfg]`, so there's no runtime dispatch cost and
//! no trait object needed (see PLAN.md section 19).
//!
//! Not every platform difference belongs here. The DNS-fix pointer (Acrylic on
//! Windows, Dnsmasq on Mac/Linux) is a documentation string, not behavior — `stack`
//! has zero code integration with either — so it's a `#[cfg]`-gated `&str` constant
//! wherever it's printed, not a function in this module. Shell-hook code (`pwsh`
//! vs. `bash`/`zsh`) is organized by shell, not OS, for the same reason: PowerShell
//! runs on Mac/Linux too, and bash runs on Windows via Git Bash — shell and OS are
//! orthogonal axes, so that code doesn't live here either.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
