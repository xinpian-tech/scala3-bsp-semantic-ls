//! Port of the Scala `SymbolStringsSuite`.

use ls_semanticdb::symbols::{self, Descriptor};

fn term(n: &str) -> Descriptor {
    Descriptor::Term(n.into())
}
fn ty(n: &str) -> Descriptor {
    Descriptor::Type(n.into())
}
fn pkg(n: &str) -> Descriptor {
    Descriptor::Package(n.into())
}
fn method(n: &str, d: &str) -> Descriptor {
    Descriptor::Method(n.into(), d.into())
}
fn split(sym: &str) -> Option<(String, Descriptor)> {
    symbols::split_last(sym)
}
fn some(owner: &str, d: Descriptor) -> Option<(String, Descriptor)> {
    Some((owner.to_string(), d))
}

#[test]
fn is_local_is_global() {
    assert!(symbols::is_local("local0"));
    assert!(symbols::is_local("local12345"));
    assert!(!symbols::is_local("locale/")); // package named locale
    assert!(!symbols::is_local("localizer.")); // term starting with local
    assert!(!symbols::is_local("a/b/C#"));
    assert!(!symbols::is_local(""));
    assert!(symbols::is_global("a/b/C#"));
    assert!(symbols::is_global("_root_/"));
    assert!(!symbols::is_global("local1"));
    assert!(!symbols::is_global(""));
}

#[test]
fn split_last_terms_types_packages() {
    assert_eq!(split("scala/Predef."), some("scala/", term("Predef")));
    assert_eq!(split("a/b/C#"), some("a/b/", ty("C")));
    assert_eq!(split("a/b/"), some("a/", pkg("b")));
    assert_eq!(split("_root_/"), some("", pkg("_root_")));
}

#[test]
fn split_last_methods_with_disambiguators() {
    assert_eq!(split("a/b/C#f()."), some("a/b/C#", method("f", "()")));
    assert_eq!(split("a/b/C#f(+2)."), some("a/b/C#", method("f", "(+2)")));
    assert_eq!(
        split("a/b/C#`<init>`()."),
        some("a/b/C#", method("<init>", "()"))
    );
    assert_eq!(split("a/A.`+`(+1)."), some("a/A.", method("+", "(+1)")));
}

#[test]
fn split_last_parameters_and_type_parameters() {
    assert_eq!(
        split("a/b/C#m().(x)"),
        some("a/b/C#m().", Descriptor::Parameter("x".into()))
    );
    assert_eq!(
        split("a/b/C#[T]"),
        some("a/b/C#", Descriptor::TypeParameter("T".into()))
    );
}

#[test]
fn split_last_backticked_names() {
    assert_eq!(split("a/`x y`."), some("a/", term("x y")));
    assert_eq!(split("a/B#`x_=`()."), some("a/B#", method("x_=", "()")));
    assert_eq!(split("a/`weird(name)`."), some("a/", term("weird(name)")));
}

#[test]
fn split_last_rejects_locals_empty_malformed() {
    assert_eq!(split("local3"), None);
    assert_eq!(split(""), None);
    assert_eq!(split("#"), None);
    assert_eq!(split("no-terminator"), None);
}

#[test]
fn display_name() {
    assert_eq!(symbols::display_name("a/b/C#").as_deref(), Some("C"));
    assert_eq!(
        symbols::display_name("a/b/C#`<init>`().").as_deref(),
        Some("<init>")
    );
    assert_eq!(symbols::display_name("_root_/").as_deref(), Some("_root_"));
    assert_eq!(symbols::display_name("local9"), None);
}

#[test]
fn owner_chain_nested() {
    assert_eq!(
        symbols::owner_chain("a/b/C#m()."),
        vec!["a/", "a/b/", "a/b/C#", "a/b/C#m()."]
    );
    assert_eq!(
        symbols::owner_chain("a/Outer.Inner.f()."),
        vec!["a/", "a/Outer.", "a/Outer.Inner.", "a/Outer.Inner.f()."]
    );
    assert_eq!(symbols::owner_chain("local2"), vec!["local2"]);
}

#[test]
fn owner() {
    assert_eq!(symbols::owner("a/b/C#").as_deref(), Some("a/b/"));
    assert_eq!(symbols::owner("a/"), None);
    assert_eq!(symbols::owner("local1"), None);
}

#[test]
fn package_name() {
    assert_eq!(
        symbols::package_name("a/b/C#D#m().").as_deref(),
        Some("a.b")
    );
    assert_eq!(symbols::package_name("_empty_/X#"), None);
    assert_eq!(
        symbols::package_name("scala/Predef.println().").as_deref(),
        Some("scala")
    );
    assert_eq!(symbols::package_name("local1"), None);
    assert_eq!(symbols::package_name("a/"), None);
}

#[test]
fn owner_name_is_nearest_enclosing_non_package() {
    assert_eq!(symbols::owner_name("a/b/C#D#").as_deref(), Some("C"));
    assert_eq!(symbols::owner_name("a/b/C#"), None);
    assert_eq!(symbols::owner_name("a/b/C#m().").as_deref(), Some("C"));
    assert_eq!(symbols::owner_name("a/b/C#m().(x)").as_deref(), Some("m"));
    assert_eq!(symbols::owner_name("local4"), None);
}

#[test]
fn companion_pair_detection() {
    assert_eq!(symbols::companion("a/b/C#").as_deref(), Some("a/b/C."));
    assert_eq!(symbols::companion("a/b/C.").as_deref(), Some("a/b/C#"));
    assert_eq!(symbols::companion("a/b/C#m()."), None);
    assert_eq!(symbols::companion("a/b/"), None);
    assert_eq!(symbols::companion("a/`x y`#").as_deref(), Some("a/`x y`."));
    assert!(symbols::is_companion_pair("a/b/C#", "a/b/C."));
    assert!(symbols::is_companion_pair("a/b/C.", "a/b/C#"));
    assert!(!symbols::is_companion_pair("a/b/C#", "a/b/D."));
}

#[test]
fn constructor_detection() {
    assert!(symbols::is_constructor("a/B#`<init>`()."));
    assert!(symbols::is_constructor("a/B#`<init>`(+1)."));
    assert!(!symbols::is_constructor("a/B#init()."));
    assert!(!symbols::is_constructor("a/B#"));
}

#[test]
fn setter_detection() {
    assert!(symbols::is_setter("a/B#`value_=`()."));
    assert_eq!(
        symbols::setter_target_name("a/B#`value_=`().").as_deref(),
        Some("value")
    );
    assert!(!symbols::is_setter("a/B#value()."));
    assert!(!symbols::is_setter("a/B#`_=`().")); // no base name
    assert_eq!(symbols::setter_target_name("a/B#value()."), None);
}

#[test]
fn enclosing_top_level() {
    assert_eq!(
        symbols::enclosing_top_level("a/b/C#D#m().").as_deref(),
        Some("a/b/C#")
    );
    assert_eq!(
        symbols::enclosing_top_level("a/b/C#").as_deref(),
        Some("a/b/C#")
    );
    assert_eq!(
        symbols::enclosing_top_level("a/b/Obj.f().").as_deref(),
        Some("a/b/Obj.")
    );
    assert_eq!(symbols::enclosing_top_level("a/b/"), None);
    assert_eq!(symbols::enclosing_top_level("local7"), None);
}

#[test]
fn encode_name() {
    assert_eq!(symbols::encode_name("plain"), "plain");
    assert_eq!(symbols::encode_name("x_="), "`x_=`");
    assert_eq!(symbols::encode_name("<init>"), "`<init>`");
    assert_eq!(symbols::encode_name("x y"), "`x y`");
    assert_eq!(symbols::encode_name(""), "``");
}

#[test]
fn is_package() {
    assert!(symbols::is_package("a/b/"));
    assert!(!symbols::is_package("a/b/C#"));
    assert!(!symbols::is_package(""));
}
