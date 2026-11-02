//! `fleet project add|list`: edit the `-- fleet:projects` region of init.lua.

use crate::config::{self, project_block, Config, Project};
use crate::worktrees::git;
use anyhow::{bail, Context, Result};
use std::path::Path;

pub const MARK_START: &str = "-- fleet:projects";
pub const MARK_END: &str = "-- fleet:projects:end";

/// Build a `Project` from a repo path: name = folder name, reviews from `origin`.
pub async fn detect(
    path: &Path,
    name: Option<String>,
    reviews: Option<String>,
    worktrees: Option<&Path>,
    base: Option<String>,
) -> Result<Project> {
    let repo = git::toplevel(path)
        .await
        .with_context(|| format!("{} is not inside a git repo", path.display()))?;
    let repo = repo.canonicalize().unwrap_or(repo);
    let name = name.unwrap_or_else(|| {
        repo.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into())
    });
    let reviews = match reviews {
        Some(r) => Some(r),
        None => git::git(&repo, &["remote", "get-url", "origin"])
            .await
            .ok()
            .and_then(|u| config::review_source_from_remote(&u)),
    };
    let base = match base {
        Some(b) => b,
        None => git::git(
            &repo,
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        )
        .await
        .ok()
        .and_then(|s| s.trim().strip_prefix("origin/").map(String::from))
        .unwrap_or_else(|| "main".into()),
    };
    Ok(Project {
        name,
        repo,
        worktrees: worktrees.map(Path::to_path_buf),
        reviews,
        base,
    })
}

/// Insert a project block before `-- fleet:projects:end` in the config file. Creates the
/// file from the example when it does not exist.
pub fn add_to_config(cfg: &Config, path: &Path, p: &Project) -> Result<()> {
    if cfg.project(&p.name).is_some() {
        bail!("project `{}` already exists in {}", p.name, path.display());
    }
    if cfg.projects.iter().any(|x| x.repo == p.repo) {
        bail!(
            "{} is already configured as project `{}`",
            p.repo.display(),
            cfg.projects.iter().find(|x| x.repo == p.repo).unwrap().name
        );
    }
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let s = Config::example(std::slice::from_ref(p));
            std::fs::write(path, &s)?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let Some(end) = src.find(MARK_END) else {
        bail!(
            "{} has no `{MARK_END}` marker; add the block by hand:\n{}",
            path.display(),
            project_block(p)
        );
    };
    // Insert before the line that holds the end marker: find the `},` closing the projects
    // table just above it.
    let before = &src[..end];
    let close = before
        .rfind("},")
        .or_else(|| before.rfind('}'))
        .context("projects table not found above the end marker")?;
    let mut out = String::with_capacity(src.len() + 256);
    out.push_str(&src[..close]);
    out.push_str(&project_block(p));
    out.push_str(&src[close..]);
    // Validate before writing.
    Config::from_lua(&out, &path.to_string_lossy())
        .context("the edited config no longer parses; nothing written")?;
    std::fs::write(path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_inside_markers() {
        let dir = std::env::temp_dir().join(format!("fleet-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("init.lua");
        let a = Project {
            name: "a".into(),
            repo: "/r/a".into(),
            worktrees: None,
            reviews: Some("gitlab:x/a".into()),
            base: "main".into(),
        };
        std::fs::write(&path, Config::example(&[a.clone()])).unwrap();
        let cfg = Config::load_from(&path).unwrap();
        let b = Project {
            name: "b".into(),
            repo: "/r/b".into(),
            worktrees: Some("/w/b".into()),
            reviews: Some("github:x/b".into()),
            base: "develop".into(),
        };
        add_to_config(&cfg, &path, &b).unwrap();
        let cfg2 = Config::load_from(&path).unwrap();
        assert_eq!(
            cfg2.projects
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(cfg2.projects[1].base, "develop");
        assert!(add_to_config(&cfg2, &path, &b).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
