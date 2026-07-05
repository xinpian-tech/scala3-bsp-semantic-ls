//! Opaque numeric identifiers.
//!
//! Two families, mirroring the Scala `ls.index` ids:
//!
//! * **Persistent ids** (`SymbolId`, `DocId`, `TargetId`, `RefGroupId`,
//!   `RenameGroupId`) are stable keys that historically survived snapshots.
//!   In the v2 storage model the stable keys are the strings themselves and
//!   these ids are a thin convenience over the durable numbering.
//! * **Snapshot ordinals** (`SymbolOrd`, `DocOrd`, `TargetOrd`, `RefGroupOrd`,
//!   `RenameGroupOrd`) are dense indices valid only within a single snapshot,
//!   used for O(1) array lookup on the query hot path.
//!
//! Each is a `#[repr(transparent)]` newtype so it is a zero-cost wrapper that
//! still refuses accidental cross-assignment between id spaces.

macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Wrap a raw durable value.
            #[inline]
            pub const fn new(v: u64) -> Self {
                $name(v)
            }

            /// The underlying durable value.
            #[inline]
            pub const fn value(self) -> u64 {
                self.0
            }
        }

        impl ::core::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl From<u64> for $name {
            #[inline]
            fn from(v: u64) -> Self {
                $name(v)
            }
        }
    };
}

macro_rules! ord_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name(u32);

        impl $name {
            /// Wrap a raw dense ordinal.
            #[inline]
            pub const fn new(v: u32) -> Self {
                $name(v)
            }

            /// The underlying dense ordinal.
            #[inline]
            pub const fn ord(self) -> u32 {
                self.0
            }

            /// The ordinal widened to a `usize` for array indexing.
            #[inline]
            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl ::core::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl From<u32> for $name {
            #[inline]
            fn from(v: u32) -> Self {
                $name(v)
            }
        }
    };
}

id_newtype!(
    /// Durable identity of a SemanticDB symbol.
    SymbolId
);
id_newtype!(
    /// Durable identity of an indexed document.
    DocId
);
id_newtype!(
    /// Durable identity of a build target.
    TargetId
);
id_newtype!(
    /// Durable identity of a reference group.
    RefGroupId
);
id_newtype!(
    /// Durable identity of a rename group.
    RenameGroupId
);

ord_newtype!(
    /// Dense per-snapshot ordinal of a symbol.
    SymbolOrd
);
ord_newtype!(
    /// Dense per-snapshot ordinal of a document.
    DocOrd
);
ord_newtype!(
    /// Dense per-snapshot ordinal of a build target.
    TargetOrd
);
ord_newtype!(
    /// Dense per-snapshot ordinal of a reference group.
    RefGroupOrd
);
ord_newtype!(
    /// Dense per-snapshot ordinal of a rename group.
    RenameGroupOrd
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_round_trips_value() {
        assert_eq!(SymbolId::new(42).value(), 42);
        assert_eq!(DocId::from(7).value(), 7);
        assert_eq!(TargetId::new(u64::MAX).value(), u64::MAX);
    }

    #[test]
    fn ord_round_trips_and_indexes() {
        assert_eq!(SymbolOrd::new(3).ord(), 3);
        assert_eq!(DocOrd::new(9).index(), 9usize);
        assert_eq!(TargetOrd::from(0).ord(), 0);
    }

    #[test]
    fn ids_order_and_compare() {
        assert!(SymbolId::new(1) < SymbolId::new(2));
        assert_eq!(RefGroupOrd::new(5), RefGroupOrd::new(5));
        assert_eq!(format!("{:?}", TargetId::new(11)), "TargetId(11)");
    }
}
