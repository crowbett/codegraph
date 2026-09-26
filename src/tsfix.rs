//! Work around a tree-sitter-typescript ambiguity: object-type members separated only by a
//! newline, where the next member is a call or construct signature,
//!
//! ```ts
//! interface Factory {
//!     <T>(fn: T): T          // the next line starts with `<` or `(`
//!     (name: string): Thing
//! }
//! ```
//!
//! parse as one member (`T<T>(...)` / `T(...)`), and error recovery then swallows the rest
//! of the file (every later declaration is lost). TypeScript itself inserts a separator
//! there. We do the same on a copy of the source when a first parse has errors: append `;`
//! to such a line. Only bytes at line ends change, so every row/column tree-sitter reports
//! for the patched copy is the same as in the original.

/// A patched copy of `src` with `;` appended to member lines that are followed by a
/// signature line, or `None` when no line needed it.
pub(crate) fn separate_signatures(src: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(src).ok()?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len() + 64);
    let mut changed = false;
    for (i, line) in lines.iter().enumerate() {
        let (body, eol) = split_eol(line);
        out.push_str(body);
        if needs_separator(body) && next_starts_signature(&lines[i + 1..]) {
            out.push(';');
            changed = true;
        }
        out.push_str(eol);
    }
    changed.then(|| out.into_bytes())
}

fn split_eol(line: &str) -> (&str, &str) {
    if let Some(b) = line.strip_suffix("\r\n") {
        (b, "\r\n")
    } else if let Some(b) = line.strip_suffix('\n') {
        (b, "\n")
    } else {
        (line, "")
    }
}

/// The code part of a line (no trailing `// comment`), trimmed.
fn code(line: &str) -> &str {
    let l = match line.find("//") {
        // Crude but safe: a `//` inside a string only means we skip a patch.
        Some(i) => &line[..i],
        None => line,
    };
    l.trim()
}

/// Ends like a type (`): T`, `): Promise<X>`, `x: number`, `]`) and not like a statement
/// that already ends or continues.
fn needs_separator(line: &str) -> bool {
    let c = code(line);
    if c.is_empty() {
        return false;
    }
    let last = c.chars().last().unwrap_or(' ');
    let ends_type = last.is_alphanumeric() || matches!(last, '_' | '$' | '>' | ']' | ')');
    ends_type
        && c.contains(':')
        && !c.ends_with("=>")
        && !matches!(last, ';' | ',' | '{' | '(' | '[' | '&' | '|')
}

/// The next non-blank, non-comment line starts a call / construct signature.
fn next_starts_signature(rest: &[&str]) -> bool {
    for l in rest {
        let t = l.trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
            continue;
        }
        return t.starts_with('<')
            || t.starts_with('(')
            || t.starts_with("new (")
            || t.starts_with("new<");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_only_signature_boundaries() {
        let src = "interface F {\n    <T>(fn: T): T\n    // c\n    (n: string): X\n}\nconst a = f(x)\n(g)\n";
        let out = String::from_utf8(separate_signatures(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(
            out,
            "interface F {\n    <T>(fn: T): T;\n    // c\n    (n: string): X\n}\nconst a = f(x)\n(g)\n"
        );
        // Same line count, same prefix of every line.
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(separate_signatures(b"let a: number = 1\nfoo()\n").is_none());
    }

    #[test]
    fn crlf_is_kept() {
        let src = "interface F {\r\n  (a: A): B\r\n  (b: B): A\r\n}\r\n";
        let out = String::from_utf8(separate_signatures(src.as_bytes()).unwrap()).unwrap();
        assert_eq!(out, "interface F {\r\n  (a: A): B;\r\n  (b: B): A\r\n}\r\n");
    }
}
