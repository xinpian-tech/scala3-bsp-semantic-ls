//! Helpers over the SemanticDB symbol grammar (scalameta spec, "Symbol"):
//!
//! ```text
//! Symbol       = GlobalSymbol | LocalSymbol
//! LocalSymbol  = "local" Number
//! GlobalSymbol = Owner Descriptor
//! Descriptor   = Name "."   (term) | Name Disambiguator "."  (method)
//!              | Name "#"   (type) | Name "/"  (package)
//!              | "(" Name ")" (parameter) | "[" Name "]" (type parameter)
//! Name         = identifier | "`" anything "`"
//! ```
//!
//! Parsing works backwards from the end of the string, so backticked names
//! containing descriptor characters are handled correctly. All indexing is over
//! `char`s (not bytes) to mirror the Scala string semantics for multi-byte names.

pub const ROOT_PACKAGE: &str = "_root_/";
pub const EMPTY_PACKAGE: &str = "_empty_/";
pub const CONSTRUCTOR_NAME: &str = "<init>";

/// The last descriptor of a global symbol, carrying its decoded name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Descriptor {
    Term(String),
    /// `(name, disambiguator)`, e.g. `Method("f", "(+1)")`.
    Method(String, String),
    Type(String),
    Package(String),
    Parameter(String),
    TypeParameter(String),
}

impl Descriptor {
    /// The decoded name of this descriptor.
    pub fn name(&self) -> &str {
        match self {
            Descriptor::Term(n)
            | Descriptor::Type(n)
            | Descriptor::Package(n)
            | Descriptor::Parameter(n)
            | Descriptor::TypeParameter(n)
            | Descriptor::Method(n, _) => n,
        }
    }
}

fn is_name_boundary(c: char) -> bool {
    matches!(c, '/' | '.' | '#' | '(' | ')' | '[' | ']' | '`')
}

/// Local symbols are `local` + id and only meaningful inside one document.
pub fn is_local(symbol: &str) -> bool {
    symbol.chars().count() > 5
        && symbol.starts_with("local")
        && !symbol.chars().any(is_name_boundary)
}

pub fn is_global(symbol: &str) -> bool {
    !symbol.is_empty() && !is_local(symbol)
}

pub fn is_package(symbol: &str) -> bool {
    symbol.ends_with('/')
}

/// Approximates `Character.isJavaIdentifierStart`: a letter, `_` or `$`.
fn is_java_id_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

/// Approximates `Character.isJavaIdentifierPart`: identifier start or a digit.
fn is_java_id_part(c: char) -> bool {
    is_java_id_start(c) || c.is_numeric()
}

/// Encodes a display name into descriptor syntax, mirroring scalameta:
/// backticks unless the name is a plain Java-style identifier.
pub fn encode_name(name: &str) -> String {
    match name.chars().next() {
        None => "``".to_string(),
        Some(head) => {
            let plain = is_java_id_start(head) && name.chars().all(is_java_id_part);
            if plain {
                name.to_string()
            } else {
                format!("`{name}`")
            }
        }
    }
}

/// `symbol.substring(0, to)` over char indices.
fn prefix(chars: &[char], to: usize) -> String {
    chars[..to].iter().collect()
}

/// Last index of `target` in `chars[..=from]`, scanning backwards; `None` if
/// absent or `from < 0`.
fn last_index_of(chars: &[char], target: char, from: i64) -> Option<usize> {
    let mut i = from;
    while i >= 0 {
        if chars[i as usize] == target {
            return Some(i as usize);
        }
        i -= 1;
    }
    None
}

/// Reads a (possibly backticked) name ending at `end_idx` inclusive. Returns the
/// start index of the name (including the opening backtick) and the decoded name.
fn read_name_backwards(chars: &[char], end_idx: i64) -> Option<(usize, String)> {
    if end_idx < 0 {
        return None;
    }
    let end = end_idx as usize;
    if chars[end] == '`' {
        let open = last_index_of(chars, '`', end_idx - 1)?;
        Some((open, chars[open + 1..end].iter().collect()))
    } else {
        let mut i = end_idx;
        while i >= 0 && !is_name_boundary(chars[i as usize]) {
            i -= 1;
        }
        if i == end_idx {
            None // empty name
        } else {
            let start = (i + 1) as usize;
            Some((start, chars[start..=end].iter().collect()))
        }
    }
}

/// Splits a global symbol into (owner prefix, last descriptor). `None` for local,
/// empty, or malformed symbols.
pub fn split_last(symbol: &str) -> Option<(String, Descriptor)> {
    if symbol.is_empty() || is_local(symbol) {
        return None;
    }
    let chars: Vec<char> = symbol.chars().collect();
    let n = chars.len();
    match chars[n - 1] {
        '/' => read_name_backwards(&chars, n as i64 - 2)
            .map(|(start, name)| (prefix(&chars, start), Descriptor::Package(name))),
        '#' => read_name_backwards(&chars, n as i64 - 2)
            .map(|(start, name)| (prefix(&chars, start), Descriptor::Type(name))),
        '.' => {
            if n >= 2 && chars[n - 2] == ')' {
                // Method: name, disambiguator "(...)", "." — the disambiguator
                // never contains parens, so the nearest '(' closes it.
                let open = last_index_of(&chars, '(', n as i64 - 2)?;
                let disambiguator: String = chars[open..n - 1].iter().collect();
                read_name_backwards(&chars, open as i64 - 1).map(|(start, name)| {
                    (
                        prefix(&chars, start),
                        Descriptor::Method(name, disambiguator),
                    )
                })
            } else {
                read_name_backwards(&chars, n as i64 - 2)
                    .map(|(start, name)| (prefix(&chars, start), Descriptor::Term(name)))
            }
        }
        ')' => read_name_backwards(&chars, n as i64 - 2).and_then(|(start, name)| {
            if start > 0 && chars[start - 1] == '(' {
                Some((prefix(&chars, start - 1), Descriptor::Parameter(name)))
            } else {
                None
            }
        }),
        ']' => read_name_backwards(&chars, n as i64 - 2).and_then(|(start, name)| {
            if start > 0 && chars[start - 1] == '[' {
                Some((prefix(&chars, start - 1), Descriptor::TypeParameter(name)))
            } else {
                None
            }
        }),
        _ => None,
    }
}

pub fn descriptor_of(symbol: &str) -> Option<Descriptor> {
    split_last(symbol).map(|(_, d)| d)
}

/// Decoded name of the last descriptor. `None` for locals and malformed symbols.
pub fn display_name(symbol: &str) -> Option<String> {
    descriptor_of(symbol).map(|d| d.name().to_string())
}

/// Owner prefix, `None` when the symbol is top-level (owner is the root).
pub fn owner(symbol: &str) -> Option<String> {
    split_last(symbol).map(|(o, _)| o).filter(|o| !o.is_empty())
}

/// All enclosing symbols from outermost to the symbol itself (inclusive). For
/// locals, just the local symbol.
pub fn owner_chain(symbol: &str) -> Vec<String> {
    if symbol.is_empty() {
        return Vec::new();
    }
    if is_local(symbol) {
        return vec![symbol.to_string()];
    }
    let mut acc: Vec<String> = Vec::new();
    let mut cur = symbol.to_string();
    loop {
        if cur.is_empty() {
            break;
        }
        acc.push(cur.clone());
        match split_last(&cur) {
            Some((owner_prefix, _)) => cur = owner_prefix,
            None => break,
        }
    }
    acc.reverse();
    acc
}

/// Dotted package path of the strictly enclosing packages. `None` for locals,
/// the root package and the empty package.
pub fn package_name(symbol: &str) -> Option<String> {
    let chain = owner_chain(symbol);
    let enclosing = &chain[..chain.len().saturating_sub(1)];
    let names: Vec<String> = enclosing
        .iter()
        .take_while(|s| is_package(s))
        .filter_map(|s| display_name(s))
        .filter(|n| n != "_root_" && n != "_empty_")
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join("."))
    }
}

/// Display name of the nearest enclosing non-package declaration. `None` when the
/// symbol sits directly in a package, and for locals.
pub fn owner_name(symbol: &str) -> Option<String> {
    let chain = owner_chain(symbol);
    let enclosing = &chain[..chain.len().saturating_sub(1)];
    enclosing
        .iter()
        .rev()
        .find(|s| !is_package(s))
        .and_then(|s| display_name(s))
}

/// Companion symbol per the grammar: `X#` <-> `X.`. Existence is the caller's
/// concern.
pub fn companion(symbol: &str) -> Option<String> {
    split_last(symbol).and_then(|(owner_prefix, d)| match d {
        Descriptor::Type(name) => Some(format!("{owner_prefix}{}.", encode_name(&name))),
        Descriptor::Term(name) => Some(format!("{owner_prefix}{}#", encode_name(&name))),
        _ => None,
    })
}

pub fn is_companion_pair(a: &str, b: &str) -> bool {
    companion(a).as_deref() == Some(b)
}

pub fn is_constructor(symbol: &str) -> bool {
    matches!(descriptor_of(symbol), Some(Descriptor::Method(name, _)) if name == CONSTRUCTOR_NAME)
}

pub fn is_setter(symbol: &str) -> bool {
    matches!(
        descriptor_of(symbol),
        Some(Descriptor::Method(name, _)) if name.chars().count() > 2 && name.ends_with("_=")
    )
}

/// For a setter `x_=(...)` the plain name `x` of its getter/field.
pub fn setter_target_name(symbol: &str) -> Option<String> {
    match descriptor_of(symbol) {
        Some(Descriptor::Method(name, _)) if name.chars().count() > 2 && name.ends_with("_=") => {
            let mut n = name;
            n.truncate(n.len() - 2); // drop the ASCII "_="
            Some(n)
        }
        _ => None,
    }
}

/// The outermost non-package enclosing symbol — the top-level class, trait or
/// object containing this symbol (the symbol itself when top-level). `None` for
/// packages and locals.
pub fn enclosing_top_level(symbol: &str) -> Option<String> {
    owner_chain(symbol)
        .into_iter()
        .find(|s| !is_package(s) && !is_local(s))
}
