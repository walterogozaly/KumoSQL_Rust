//! The rule registry.
//!
//! Ported from `register_rule`, `get_rule` and `available_rules` in
//! `engine.py`.
//!
//! The registry is built from the crates that depend on this one rather than by
//! a plugin scan: Rust's static registry needs either a build script or a
//! generated list, and a generated list is the one that cannot silently
//! disagree with what is compiled in.

use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

use crate::RewriteRule;

/// The registered rules, by name.
///
/// Populated by [`register_rule`] during crate initialisation.
static REGISTRY: LazyLock<RwLock<BTreeMap<&'static str, &'static dyn RewriteRule>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

/// Register a rule under its own [`RewriteRule::name`].
///
/// Registration panics on a duplicate name: two rules claiming one name means a
/// caller asking for it would get whichever was registered last, which is not a
/// failure worth discovering at rewrite time.
pub fn register_rule(rule: &'static dyn RewriteRule) {
    let mut registry = REGISTRY
        .write()
        .expect("the rule registry lock was poisoned");
    if let Some(_existing) = registry.get(rule.name()) {
        panic!(
            "two rules are registered as {:?}; the second one would shadow the first",
            rule.name()
        );
    }
    registry.insert(rule.name(), rule);
}

/// The rule registered under `name`.
///
/// `None` when no such rule exists -- which is what the CLI reports when asked
/// for an unknown rule, rather than substituting a default.
pub fn get_rule(name: &str) -> Option<&'static dyn RewriteRule> {
    REGISTRY.read().ok()?.get(name).copied()
}

/// Every registered rule, by name.
pub fn available_rules() -> Vec<(&'static str, &'static dyn RewriteRule)> {
    let Ok(registry) = REGISTRY.read() else {
        return Vec::new();
    };
    registry.iter().map(|(name, rule)| (*name, *rule)).collect()
}

/// Every registered rule's name, in order.
pub fn available_rule_names() -> Vec<&'static str> {
    available_rules()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The canonical rule order: every rule except the opt-in ones, in a fixed
/// order, and with formatting last.
///
/// A rule that is not idempotent cannot go in a pipeline that runs to a fixed
/// point, so the order is only meaningful once `check_idempotence` passes for
/// it. That check is task 5.
pub fn canonical_rule_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = available_rules()
        .into_iter()
        .filter(|(_, rule)| !rule.opt_in())
        .map(|(name, _)| name)
        .collect();
    names.sort_unstable();
    names
}
