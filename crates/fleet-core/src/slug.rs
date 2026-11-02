//! Names: slug validation, temp slugs, branch derivation.

use rand::{distributions::Alphanumeric, Rng};

pub const TYPES: &[&str] = &["feat", "fix", "update", "refactor", "docs", "chore", "test"];

pub fn is_valid(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
        })
        && !slug.starts_with('-')
        && !slug.starts_with('.')
}

/// `wip-7f3k`
pub fn temp() -> String {
    let s: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(4)
        .map(|c| (c as char).to_ascii_lowercase())
        .collect();
    format!("wip-{s}")
}

pub fn is_temp(slug: &str) -> bool {
    slug.starts_with("wip-")
}

/// `fix-sfs-content-type` -> `fix/sfs-content-type`; `thing` -> `feat/thing`; `wip-x` -> `wip/wip-x`.
pub fn branch_for(slug: &str) -> String {
    if is_temp(slug) {
        return format!("wip/{slug}");
    }
    for t in TYPES {
        if let Some(rest) = slug.strip_prefix(&format!("{t}-")) {
            if !rest.is_empty() {
                return format!("{t}/{rest}");
            }
        }
    }
    format!("feat/{slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches() {
        assert_eq!(branch_for("fix-sfs-content-type"), "fix/sfs-content-type");
        assert_eq!(branch_for("add-lev8-provider"), "feat/add-lev8-provider");
        assert_eq!(branch_for("wip-7f3k"), "wip/wip-7f3k");
        assert_eq!(branch_for("docs-readme"), "docs/readme");
        assert!(is_valid("fix-x") && !is_valid("Fix X") && !is_valid(""));
        assert!(is_temp(&temp()) && temp().len() == 8);
    }
}
