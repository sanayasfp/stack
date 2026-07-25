use std::collections::BTreeMap;

/// Resolves `{NAME}` placeholders in a command template.
///
/// Order: reserved keywords (stack-controlled, e.g. `{port}`) first and always —
/// never falls through to env/prompt, so `{PORT}` can never be silently misread as
/// "look for a PORT env var" instead of the built-in. Then the process environment
/// (picks up anything already set, including via `stack load-env`). Then, only if
/// `allow_prompt` is true, an interactive prompt — deliberately opt-in so `stack up`
/// run non-interactively (CI, a background script) fails fast instead of hanging.
///
/// On failure, returns every unresolved name at once rather than just the first —
/// failing one at a time, fixing, rerunning, is exactly the toil this tool exists to
/// eliminate elsewhere.
pub fn resolve(template: &str, reserved: &BTreeMap<String, String>, allow_prompt: bool) -> Result<String, Vec<String>> {
    let names = extract_names(template);

    let mut resolved: BTreeMap<String, String> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();

    for name in &names {
        if let Some(v) = reserved.get(name) {
            resolved.insert(name.clone(), v.clone());
        } else if let Ok(v) = std::env::var(name) {
            resolved.insert(name.clone(), v);
        } else {
            missing.push(name.clone());
        }
    }

    if !missing.is_empty() {
        if !allow_prompt {
            return Err(missing);
        }
        for name in &missing {
            let value = prompt_for(name);
            println!("tip: set {name} in your environment, or `stack load-env`, to skip this prompt next time");
            resolved.insert(name.clone(), value);
        }
    }

    let mut output = template.to_string();
    for (name, value) in &resolved {
        output = output.replace(&format!("{{{name}}}"), value);
    }
    Ok(output)
}

fn extract_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        match rest.find('}') {
            Some(end) => {
                let name = rest[..end].to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
                rest = &rest[end + 1..];
            }
            None => break,
        }
    }
    names
}

/// Masks input for placeholder names that look like secrets, so a password isn't
/// echoed to the terminal.
fn prompt_for(name: &str) -> String {
    let upper = name.to_uppercase();
    let looks_secret = ["PASSWORD", "SECRET", "TOKEN", "KEY"]
        .iter()
        .any(|kw| upper.contains(kw));

    if looks_secret {
        dialoguer::Password::new()
            .with_prompt(name)
            .interact()
            .unwrap_or_default()
    } else {
        dialoguer::Input::new()
            .with_prompt(name)
            .interact_text()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_wins_over_env_of_same_name() {
        // SAFETY: test runs single-threaded within this process; no other test reads PORT.
        unsafe { std::env::set_var("PORT", "9999") };
        let mut reserved = BTreeMap::new();
        reserved.insert("port".to_string(), "8000".to_string());
        let out = resolve("run --port={port}", &reserved, false).unwrap();
        assert_eq!(out, "run --port=8000");
        unsafe { std::env::remove_var("PORT") };
    }

    #[test]
    fn env_resolves_non_reserved_placeholder() {
        unsafe { std::env::set_var("STACK_TEST_VAR", "hello") };
        let out = resolve("echo {STACK_TEST_VAR}", &BTreeMap::new(), false).unwrap();
        assert_eq!(out, "echo hello");
        unsafe { std::env::remove_var("STACK_TEST_VAR") };
    }

    #[test]
    fn missing_without_prompt_reports_all_at_once() {
        let err = resolve("{FIRST_MISSING} {SECOND_MISSING}", &BTreeMap::new(), false).unwrap_err();
        assert_eq!(err, vec!["FIRST_MISSING".to_string(), "SECOND_MISSING".to_string()]);
    }
}
