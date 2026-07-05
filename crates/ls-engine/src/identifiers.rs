//! Scala 3 identifier validation for rename targets. A new name is either a
//! plain identifier (used verbatim), or anything backtick-quotable (wrapped in
//! backticks — keywords, spaces, operators mixed with letters, ...), or rejected
//! (empty, contains a backtick or a line break).

const KEYWORDS: &[&str] = &[
    "abstract",
    "case",
    "catch",
    "class",
    "def",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "final",
    "finally",
    "for",
    "given",
    "if",
    "implicit",
    "import",
    "lazy",
    "match",
    "new",
    "null",
    "object",
    "override",
    "package",
    "private",
    "protected",
    "return",
    "sealed",
    "super",
    "then",
    "throw",
    "trait",
    "true",
    "try",
    "type",
    "val",
    "var",
    "while",
    "with",
    "yield",
    "_",
    ":",
    "=",
    "<-",
    "=>",
    "<:",
    ">:",
    "#",
    "@",
    "=>>",
    "?=>",
];

fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

fn is_op_char(c: char) -> bool {
    // The ASCII operator set of the Scala lexer; exotic Unicode Sm/So symbols are
    // approximated away (they do not appear in practical rename targets).
    matches!(
        c,
        '!' | '#'
            | '%'
            | '&'
            | '*'
            | '+'
            | '-'
            | '/'
            | ':'
            | '<'
            | '='
            | '>'
            | '?'
            | '@'
            | '\\'
            | '^'
            | '|'
            | '~'
    )
}

fn is_id_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

fn is_id_part(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphanumeric()
}

/// Plain identifier per the Scala lexical syntax (simplified): an alphanumeric
/// identifier, optionally `_`-joined with a trailing operator part, or a pure
/// operator identifier.
pub fn is_plain_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.chars().all(is_op_char) {
        return true;
    }
    let first = name.chars().next().unwrap();
    if !is_id_start(first) {
        return false;
    }
    let underscore = name.rfind('_');
    let (alnum, op): (&str, &str) = match underscore {
        Some(u)
            if u < name.len() - 1
                && name[u + 1..].chars().all(is_op_char)
                && name[..u + 1].chars().all(is_id_part) =>
        {
            (&name[..u + 1], &name[u + 1..])
        }
        _ => (name, ""),
    };
    alnum.chars().all(is_id_part) && (op.is_empty() || op.chars().all(is_op_char))
}

/// The token to write into source for `name`: verbatim for plain non-keyword
/// identifiers, backtick-quoted when the name demands it, `Err(message)` when the
/// name cannot be a Scala identifier at all.
pub fn encode(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("new name must not be empty".to_string());
    }
    if name.chars().any(|c| c == '`' || c == '\n' || c == '\r') {
        return Err(format!("'{name}' is not a valid Scala identifier"));
    }
    if is_plain_identifier(name) && !is_keyword(name) {
        Ok(name.to_string())
    } else {
        Ok(format!("`{name}`"))
    }
}
