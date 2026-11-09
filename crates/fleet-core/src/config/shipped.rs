//! The shipped default configuration, embedded at build time from `config/default/` at the
//! repo root. `include_str!` per file (no extra crate); the test below fails if a file is
//! added to the folder without being listed here.

/// `(path relative to config/default, contents)`.
pub const FILES: &[(&str, &str)] = &[
    (
        "mod.lua",
        include_str!("../../../../config/default/mod.lua"),
    ),
    (
        "defaults/mod.lua",
        include_str!("../../../../config/default/defaults/mod.lua"),
    ),
    (
        "defaults/system.md",
        include_str!("../../../../config/default/defaults/system.md"),
    ),
    (
        "agents/mod.lua",
        include_str!("../../../../config/default/agents/mod.lua"),
    ),
    (
        "tasks/mod.lua",
        include_str!("../../../../config/default/tasks/mod.lua"),
    ),
    (
        "tasks/review.md",
        include_str!("../../../../config/default/tasks/review.md"),
    ),
    (
        "tasks/explain.md",
        include_str!("../../../../config/default/tasks/explain.md"),
    ),
    (
        "machines/mod.lua",
        include_str!("../../../../config/default/machines/mod.lua"),
    ),
    (
        "keys/mod.lua",
        include_str!("../../../../config/default/keys/mod.lua"),
    ),
];

pub fn get(path: &str) -> Option<&'static str> {
    FILES.iter().find(|(p, _)| *p == path).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file under config/default must be embedded (a new prompt file that is not listed
    /// would be silently missing from the binary).
    #[test]
    fn every_shipped_file_is_embedded() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/default");
        let mut on_disk = vec![];
        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        walk(&root, &root, &mut on_disk);
        on_disk.sort();
        let mut listed: Vec<String> = FILES.iter().map(|(p, _)| p.to_string()).collect();
        listed.sort();
        assert_eq!(on_disk, listed);
    }
}
