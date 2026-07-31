/// Green — a step succeeded.
pub fn ok(text: &str) -> String {
    console::style(text).green().to_string()
}

/// Amber — worth noticing, not a failure.
pub fn warn(text: &str) -> String {
    console::style(text).yellow().to_string()
}

/// Red — a step failed.
pub fn err(text: &str) -> String {
    console::style(text).red().to_string()
}
