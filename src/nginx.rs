use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;

const LOG_FORMAT_COMBINED: &str = r#"$remote_addr - $remote_user [$time_local] "$request" $status $body_bytes_sent "$http_referer" "$http_user_agent""#;

// We know that these patterns will compile.
static NGINX_VARIABLE_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$([a-zA-Z0-9_]+)").unwrap());
static SPECIAL_CHARS_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"([\.\*\+\?\|\(\)\{\}\[\]])").unwrap());

// TODO: Allow use of other formats for the parameter.
pub(crate) fn format_to_pattern(_format: &str) -> Result<Regex> {
    let format = LOG_FORMAT_COMBINED;

    // Escape all of the existing special characters.
    let pattern = SPECIAL_CHARS_REGEX.replace_all(format, r"\$1");

    // Name our capture groups based on their name in the specified log format.
    let captures = NGINX_VARIABLE_REGEX.replace_all(&pattern, r"(?P<$1>.*)");
    Ok(Regex::new(&captures)?)
}

// List the available variables based on the supplied log format.
pub(crate) fn available_variables(format: &str) -> Result<String> {
    Ok(format_to_pattern(format)?
        .capture_names()
        .filter_map(|c| match c {
            Some(n) => Some(n.to_string()),
            None => None,
        })
        .collect::<Vec<String>>()
        .join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_matches() {
        let line = r#"66.249.65.3 - - [06/Nov/2014:19:11:24 +0600] "GET / HTTP/1.1" 200 4223 "-" "User-Agent""#;
        let pattern = format_to_pattern(LOG_FORMAT_COMBINED).unwrap();
        assert!(pattern.captures(line).is_some());
    }
}
