//! Self-registering command modules (build.rs, AG3).

use std::collections::HashSet;

#[test]
fn every_command_module_registers_once_in_legacy_order_first() {
    let modules = crate::commands::command_modules();
    let names: Vec<&str> = modules.iter().map(|(n, _)| n.as_str()).collect();
    let unique: HashSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "a command module is registered twice: {names:?}");
    // The frozen order names existing modules only (a renamed file must leave the list).
    let legacy = crate::commands::LEGACY_ORDER;
    assert_eq!(&names[..legacy.len()], legacy, "LEGACY_ORDER names a module that doesn't exist");
    // The rest follow by file name.
    let rest: Vec<&str> = names[legacy.len()..].to_vec();
    let mut sorted = rest.clone();
    sorted.sort_by_key(|n| (n.starts_with("crate::"), n.to_string()));
    assert_eq!(rest, sorted);
    // Discovery found the modules that existed before it.
    for m in ["file", "edit", "layer", "stubs", "crate::models", "crate::preview"] {
        assert!(unique.contains(m), "{m} not registered");
    }
}

#[test]
fn command_specs_concatenate_the_modules() {
    let n: usize = crate::commands::command_modules().iter().map(|(_, specs)| specs().len()).sum();
    assert_eq!(crate::command_specs().len(), n);
    assert_eq!(crate::command_specs().first().map(|c| c.id), crate::commands::command_modules().first().and_then(|(_, specs)| specs().first().map(|c| c.id)));
}
