//! SemanticDB symbol identity, kinds, and property masks.

use crate::ids::DocId;

/// Identity of a SemanticDB symbol.
///
/// Global symbols are unique per universe; local symbols are only meaningful
/// together with their document.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct SymbolKey {
    pub semantic_symbol: String,
    pub local_doc: Option<DocId>,
}

impl SymbolKey {
    /// A global symbol key.
    pub fn global(sym: impl Into<String>) -> Self {
        SymbolKey {
            semantic_symbol: sym.into(),
            local_doc: None,
        }
    }

    /// A local symbol key, scoped to `doc`.
    pub fn local(sym: impl Into<String>, doc: DocId) -> Self {
        SymbolKey {
            semantic_symbol: sym.into(),
            local_doc: Some(doc),
        }
    }

    /// Is this a document-local symbol?
    #[inline]
    pub fn is_local(&self) -> bool {
        self.local_doc.is_some()
    }
}

/// The subset of SemanticDB `SymbolInformation.Kind` we materialize.
///
/// [`SymKind::code`] follows the SemanticDB spec numbering so persisted rows
/// stay debuggable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SymKind {
    UnknownKind,
    LocalValue,
    LocalVariable,
    Method,
    Constructor,
    Macro,
    Type,
    Parameter,
    SelfParameter,
    TypeParameter,
    Object,
    Package,
    PackageObject,
    Class,
    Trait,
    Interface,
    Field,
}

impl SymKind {
    /// The SemanticDB spec code for this kind.
    pub const fn code(self) -> i32 {
        match self {
            SymKind::UnknownKind => 0,
            SymKind::LocalValue => 19,
            SymKind::LocalVariable => 20,
            SymKind::Method => 3,
            SymKind::Constructor => 21,
            SymKind::Macro => 6,
            SymKind::Type => 7,
            SymKind::Parameter => 8,
            SymKind::SelfParameter => 17,
            SymKind::TypeParameter => 9,
            SymKind::Object => 10,
            SymKind::Package => 11,
            SymKind::PackageObject => 12,
            SymKind::Class => 13,
            SymKind::Trait => 14,
            SymKind::Interface => 18,
            SymKind::Field => 15,
        }
    }

    /// Recover a kind from its SemanticDB spec code; unknown codes fold to
    /// [`SymKind::UnknownKind`], matching the Scala `SymKind.fromCode`.
    pub const fn from_code(code: i32) -> SymKind {
        match code {
            19 => SymKind::LocalValue,
            20 => SymKind::LocalVariable,
            3 => SymKind::Method,
            21 => SymKind::Constructor,
            6 => SymKind::Macro,
            7 => SymKind::Type,
            8 => SymKind::Parameter,
            17 => SymKind::SelfParameter,
            9 => SymKind::TypeParameter,
            10 => SymKind::Object,
            11 => SymKind::Package,
            12 => SymKind::PackageObject,
            13 => SymKind::Class,
            14 => SymKind::Trait,
            18 => SymKind::Interface,
            15 => SymKind::Field,
            _ => SymKind::UnknownKind,
        }
    }
}

/// SemanticDB `SymbolInformation.Property` bit mask (spec numbering).
///
/// A raw `u32` carries the combined property set.
pub mod sym_props {
    pub const ABSTRACT: u32 = 0x4;
    pub const FINAL: u32 = 0x8;
    pub const SEALED: u32 = 0x10;
    pub const IMPLICIT: u32 = 0x20;
    pub const LAZY: u32 = 0x40;
    pub const CASE: u32 = 0x80;
    pub const COVARIANT: u32 = 0x100;
    pub const CONTRAVARIANT: u32 = 0x200;
    pub const VAL: u32 = 0x400;
    pub const VAR: u32 = 0x800;
    pub const STATIC: u32 = 0x1000;
    pub const PRIMARY: u32 = 0x2000;
    pub const ENUM: u32 = 0x4000;
    pub const DEFAULT: u32 = 0x8000;
    pub const GIVEN: u32 = 0x10000;
    pub const INLINE: u32 = 0x20000;
    pub const OPEN: u32 = 0x40000;
    pub const TRANSPARENT: u32 = 0x80000;
    pub const INFIX: u32 = 0x100000;
    pub const OPAQUE: u32 = 0x200000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_key_global_and_local() {
        let g = SymbolKey::global("scala/Predef.");
        assert!(!g.is_local());
        assert_eq!(g.local_doc, None);

        let l = SymbolKey::local("local0", DocId::new(4));
        assert!(l.is_local());
        assert_eq!(l.local_doc, Some(DocId::new(4)));
    }

    #[test]
    fn symkind_code_round_trips() {
        for k in [
            SymKind::UnknownKind,
            SymKind::LocalValue,
            SymKind::LocalVariable,
            SymKind::Method,
            SymKind::Constructor,
            SymKind::Macro,
            SymKind::Type,
            SymKind::Parameter,
            SymKind::SelfParameter,
            SymKind::TypeParameter,
            SymKind::Object,
            SymKind::Package,
            SymKind::PackageObject,
            SymKind::Class,
            SymKind::Trait,
            SymKind::Interface,
            SymKind::Field,
        ] {
            assert_eq!(SymKind::from_code(k.code()), k);
        }
    }

    #[test]
    fn symkind_unknown_code_folds_to_unknown() {
        assert_eq!(SymKind::from_code(9999), SymKind::UnknownKind);
        assert_eq!(SymKind::from_code(-1), SymKind::UnknownKind);
    }

    #[test]
    fn sym_props_are_disjoint_single_bits() {
        for p in [
            sym_props::ABSTRACT,
            sym_props::FINAL,
            sym_props::SEALED,
            sym_props::OPAQUE,
            sym_props::GIVEN,
        ] {
            assert_eq!(p.count_ones(), 1, "each property is a single bit");
        }
    }
}
