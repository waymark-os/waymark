use fancy_regex::Regex;

// This hits, in order:
// • Any character of []:`{}#'";()|$,.!?=
// • Any digit (\d)
// • Any whitespace (\s)
// • Case-insensitive sign-insensitive float "keywords" inf, infinity and nan.
thread_local! {
    // Embedders may run each interpreter/process in a resettable allocation
    // domain. Keeping this cache in process-wide LazyLock would let first use
    // publish domain-owned allocations into a global root. TLS gives every
    // interpreter thread its own cache and drops it when that thread exits.
    static NEEDS_QUOTING_REGEX: Regex =
        Regex::new(r#"[\[\]:`\{\}#'";\(\)\|\$,\.\d\s!?=]|(?i)^[+\-]?(inf(inity)?|nan)$"#)
            .unwrap_or_else(|error| panic!("internal error: NEEDS_QUOTING_REGEX didn't compile: {error}"));
}

pub fn needs_quoting(string: &str) -> bool {
    if string.is_empty() {
        return true;
    }
    // These are case-sensitive keywords
    match string {
        // `true`/`false`/`null` are active keywords in JSON and NUON
        // `&&` is denied by the nu parser for diagnostics reasons
        // (https://github.com/nushell/nushell/pull/7241)
        "true" | "false" | "null" | "&&" => return true,
        _ => (),
    };
    // All other cases are handled here
    NEEDS_QUOTING_REGEX.with(|regex| regex.is_match(string).unwrap_or(false))
}

pub fn escape_quote_string(string: &str) -> String {
    let mut output = String::with_capacity(string.len() + 2);
    output.push('"');

    for c in string.chars() {
        if c == '"' || c == '\\' {
            output.push('\\');
        }
        output.push(c);
    }

    output.push('"');
    output
}

/// Returns a raw string representation if the string contains quotes or backslashes.
/// Otherwise returns None (caller should use regular quoting or bare string).
///
/// Raw strings avoid escaping by using `r#'...'#` syntax with enough `#` characters
/// to ensure the closing delimiter is unambiguous.
///
/// Note: Nushell requires at least one `#` in raw strings (i.e., `r#'...'#` not `r'...'`).
pub fn as_raw_string(s: &str) -> Option<String> {
    // Only use raw strings if they would avoid escaping
    if !s.contains('"') && !s.contains('\\') {
        return None;
    }

    // Find minimum # count needed for delimiter
    // Nushell requires at least one #, so start at 1
    // Need to avoid `'#...#` patterns in content that would close early
    let mut hash_count = 1;
    loop {
        let closing = format!("'{}", "#".repeat(hash_count));
        if !s.contains(&closing) {
            break;
        }
        hash_count += 1;
    }

    let hashes = "#".repeat(hash_count);
    Some(format!("r{hashes}'{s}'{hashes}"))
}
