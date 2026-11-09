//! Render what an agent receives when a task starts: fleet's system text, the task's extra
//! lines, the skills to load, the task's instructions, then the user's prompt. Pure, so it
//! is tested without a harness.

use crate::config::{On, ResolvedTask};
use std::collections::BTreeMap;

/// The facts a task is started with; every one is a placeholder in config text.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub slug: String,
    pub repo: String,
    pub branch: String,
    pub base: String,
    pub agent: String,
    pub review_id: String,
    pub review_url: String,
    pub review_title: String,
    pub source_branch: String,
    pub target_branch: String,
    /// The slug is a temporary `wip-*`: ask the agent to rename the task.
    pub temp_slug: bool,
}

impl Facts {
    fn map(&self) -> BTreeMap<&'static str, &str> {
        BTreeMap::from([
            ("slug", self.slug.as_str()),
            ("repo", self.repo.as_str()),
            ("branch", self.branch.as_str()),
            ("base", self.base.as_str()),
            ("agent", self.agent.as_str()),
            ("review_id", self.review_id.as_str()),
            ("review_url", self.review_url.as_str()),
            ("review_title", self.review_title.as_str()),
            ("source_branch", self.source_branch.as_str()),
            ("target_branch", self.target_branch.as_str()),
        ])
    }
}

/// `{name}` placeholders substituted; unknown ones are left as written (Markdown braces
/// survive).
pub fn fill(text: &str, facts: &Facts) -> String {
    let map = facts.map();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}') {
            Some(j)
                if after[..j]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                match map.get(&after[..j]) {
                    Some(v) => out.push_str(v),
                    None => {
                        out.push('{');
                        out.push_str(&after[..=j]);
                    }
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What goes into the harness as *system* text (Claude: `--append-system-prompt`; others
/// prepend it to the prompt): fleet's text, the review/rename lines, the skills line, the
/// agent's and the task's instructions.
pub fn system(task: &ResolvedTask, facts: &Facts, skill_names: &[String]) -> String {
    let mut parts: Vec<String> = vec![fill(&task.system, facts)];
    if facts.temp_slug {
        parts.push(format!(
            "Before any other step, pick a short kebab-case name for this task (e.g. fix-sfs-content-type; \
             a leading feat-/fix-/update-/refactor-/docs-/chore-/test- becomes the branch type) and run: \
             fleet agent rename {} <name>",
            facts.slug
        ));
    }
    if task.on == On::Review {
        parts.push(fill(
            "Review {review_id} \"{review_title}\" ({review_url}), {source_branch} -> {target_branch}, \
             checked out detached at the review head.",
            facts,
        ));
    }
    if !skill_names.is_empty() {
        parts.push(format!(
            "Skills to read and follow first: {}.",
            skill_names.join(", ")
        ));
    }
    if let Some(i) = &task.instructions {
        let i = fill(i, facts);
        if !i.trim().is_empty() {
            parts.push(i.trim().to_string());
        }
    }
    parts.join("\n")
}

/// The whole first message when the harness has no separate system channel.
pub fn combined(system: &str, prompt: &str) -> String {
    match (system.trim().is_empty(), prompt.trim().is_empty()) {
        (true, _) => prompt.to_string(),
        (false, true) => system.to_string(),
        (false, false) => format!("{system}\n\n{prompt}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn facts() -> Facts {
        Facts {
            slug: "fix-sfs".into(),
            repo: "monid-services".into(),
            branch: "fix/sfs".into(),
            base: "main".into(),
            agent: "opencode".into(),
            ..Default::default()
        }
    }

    #[test]
    fn placeholders() {
        let f = facts();
        assert_eq!(
            fill("Task {slug} in {repo}", &f),
            "Task fix-sfs in monid-services"
        );
        assert_eq!(
            fill("keep {unknown} and {} and { x }", &f),
            "keep {unknown} and {} and { x }"
        );
        assert_eq!(fill("json {\"a\":1}", &f), "json {\"a\":1}");
    }

    #[test]
    fn system_layout_for_start_and_review() {
        let cfg = Config::from_lua(
            r#"return { agents = { opencode = { harness = "opencode" } }, defaults = { agent = "opencode" } }"#,
            "t",
        )
        .unwrap();
        let start = cfg.task("start", None).unwrap();
        let mut f = facts();
        f.slug = "wip-ab12".into();
        f.temp_slug = true;
        let s = system(&start, &f, &["implement-change".into()]);
        assert!(s.starts_with("Started by fleet. Task wip-ab12 in monid-services on fix/sfs (base main), agent opencode."));
        assert!(s.contains("fleet agent rename wip-ab12 <name>"));
        assert!(s.contains("Skills to read and follow first: implement-change."));
        assert!(!s.contains("Review "));

        let review = cfg.task("review", None).unwrap();
        let f = Facts {
            review_id: "!514".into(),
            review_title: "Add X".into(),
            review_url: "https://gl/514".into(),
            source_branch: "feat/x".into(),
            target_branch: "main".into(),
            ..facts()
        };
        let s = system(&review, &f, &[]);
        assert!(s.contains("Review !514 \"Add X\" (https://gl/514), feat/x -> main"));
        assert!(s.contains("root cause")); // the shipped review instructions
        assert!(!s.contains("Skills to read"));
        assert_eq!(combined("S", ""), "S");
        assert_eq!(combined("", "P"), "P");
        assert_eq!(combined("S", "P"), "S\n\nP");
    }
}
