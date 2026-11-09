//! Evaluate a config tree written in Lua 5.4 inside a sandbox.
//!
//! The standard library is reduced to `string`, `table`, `math` and `os.getenv`, plus two
//! helpers that stay inside the tree being evaluated:
//!
//! - `require("name")` loads `name/mod.lua` or `name.lua` **relative to the calling file's
//!   folder**, evaluates it once and returns what it returned (cached, like Lua's own).
//! - `read("file.md")` returns the text of a file next to the calling file (trailing
//!   whitespace trimmed) — the way prompt files sit beside the Lua that uses them.
//!
//! A tree is the embedded shipped config, a folder on disk (`~/.config/fleet`), or an
//! in-memory map (tests). Paths never escape it: `..` segments are rejected.

use anyhow::{bail, Result};
use mlua::{Lua, LuaSerdeExt, StdLib, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Where a tree's files come from.
#[derive(Clone)]
pub enum Tree {
    /// `(relative path, contents)` pairs (the shipped config).
    Embedded(&'static [(&'static str, &'static str)]),
    /// A folder on disk; paths are relative to it.
    Dir(PathBuf),
    /// Files given directly (tests, `fleet config --check FILE`).
    Memory(HashMap<String, String>),
}

impl Tree {
    fn read(&self, rel: &str) -> Option<String> {
        match self {
            Tree::Embedded(files) => files
                .iter()
                .find(|(p, _)| *p == rel)
                .map(|(_, c)| (*c).to_string()),
            Tree::Dir(dir) => std::fs::read_to_string(dir.join(rel)).ok(),
            Tree::Memory(m) => m.get(rel).cloned(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Tree::Embedded(_) => "<shipped>".into(),
            Tree::Dir(d) => d.display().to_string(),
            Tree::Memory(_) => "<memory>".into(),
        }
    }
}

/// `dir` + `rel`, normalised, refusing to leave the tree.
fn join(dir: &str, rel: &str) -> Result<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => bail!("`{rel}`: paths may not leave the config folder"),
            s => parts.push(s),
        }
    }
    Ok(parts.join("/"))
}

fn parent(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((d, _)) => d.to_string(),
        None => String::new(),
    }
}

/// Evaluate `entry` (e.g. `mod.lua`, `init.lua`) in `tree` and return the table it returned
/// as JSON. Lua tables with only integer keys 1..n become arrays; empty tables become `{}`.
/// mlua errors are not `Send + Sync`; carry their message.
fn ml<T>(r: mlua::Result<T>) -> Result<T> {
    r.map_err(|e| anyhow::anyhow!("{}", lua_error(e)))
}

pub fn eval(tree: &Tree, entry: &str) -> Result<serde_json::Value> {
    let tree = Rc::new(tree.clone());
    let lua = ml(Lua::new_with(
        StdLib::STRING | StdLib::TABLE | StdLib::MATH,
        mlua::LuaOptions::default(),
    ))?;
    let os = ml(lua.create_table())?;
    ml(os.set(
        "getenv",
        ml(lua.create_function(|_, k: String| Ok(std::env::var(k).ok())))?,
    ))?;
    ml(lua.globals().set("os", os))?;

    // The folder of the file being evaluated (innermost first) and the module cache.
    let dirs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(vec![parent(entry)]));
    let cache = ml(lua.create_table())?;
    ml(lua.set_named_registry_value("fleet_modules", cache))?;

    {
        let tree = tree.clone();
        let dirs = dirs.clone();
        ml(lua.globals().set(
            "read",
            ml(lua.create_function(move |_, rel: String| {
                let dir = dirs.borrow().last().cloned().unwrap_or_default();
                let path = join(&dir, &rel).map_err(mlua::Error::external)?;
                match tree.read(&path) {
                    Some(s) => Ok(s.trim_end().to_string()),
                    None => Err(mlua::Error::external(anyhow::anyhow!(
                        "read(\"{rel}\"): no file `{path}` in {}",
                        tree.describe()
                    ))),
                }
            }))?,
        ))?;
    }
    {
        let tree = tree.clone();
        let dirs = dirs.clone();
        ml(lua.globals().set(
            "require",
            ml(lua.create_function(move |lua, name: String| {
                let dir = dirs.borrow().last().cloned().unwrap_or_default();
                let candidates = [
                    join(&dir, &format!("{name}/mod.lua")),
                    join(&dir, &format!("{name}.lua")),
                ];
                let mut found = None;
                for c in candidates.iter() {
                    let c = c
                        .as_ref()
                        .map_err(|e| mlua::Error::external(anyhow::anyhow!("{e}")))?;
                    if let Some(src) = tree.read(c) {
                        found = Some((c.clone(), src));
                        break;
                    }
                }
                let Some((path, src)) = found else {
                    return Err(mlua::Error::external(anyhow::anyhow!(
                        "require(\"{name}\"): no `{name}/mod.lua` or `{name}.lua` under `{}` in {}",
                        if dir.is_empty() { "." } else { dir.as_str() },
                        tree.describe()
                    )));
                };
                let cache: mlua::Table = lua.named_registry_value("fleet_modules")?;
                if let Ok(v) = cache.get::<Value>(path.as_str()) {
                    if !matches!(v, Value::Nil) {
                        return Ok(v);
                    }
                }
                dirs.borrow_mut().push(parent(&path));
                let r: mlua::Result<Value> = lua.load(&src).set_name(&path).eval();
                dirs.borrow_mut().pop();
                let v = r?;
                cache.set(path.as_str(), v.clone())?;
                Ok(v)
            }))?,
        ))?;
    }

    let Some(src) = tree.read(entry) else {
        bail!("no `{entry}` in {}", tree.describe());
    };
    let value: Value = lua
        .load(&src)
        .set_name(entry)
        .eval()
        .map_err(|e| anyhow::anyhow!("{entry}: {}", lua_error(e)))?;
    if matches!(value, Value::Nil) {
        bail!("{entry}: the file must `return {{ ... }}` a config table");
    }
    let json: serde_json::Value = lua
        .from_value(value)
        .map_err(|e| anyhow::anyhow!("{entry}: {}", lua_error(e)))?;
    Ok(json)
}

pub fn lua_error(e: mlua::Error) -> String {
    match e {
        mlua::Error::SyntaxError { message, .. } => message,
        mlua::Error::RuntimeError(m) => m,
        mlua::Error::CallbackError { cause, .. } => lua_error((*cause).clone()),
        mlua::Error::ExternalError(e) => e.to_string(),
        other => other.to_string(),
    }
}

/// A one-file tree (tests, `--check FILE`).
pub fn memory(entry: &str, src: &str) -> Tree {
    let mut m = HashMap::new();
    m.insert(entry.to_string(), src.to_string());
    Tree::Memory(m)
}

#[allow(dead_code)]
pub fn dir(p: &Path) -> Tree {
    Tree::Dir(p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(files: &[(&str, &str)]) -> Tree {
        Tree::Memory(
            files
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        )
    }

    #[test]
    fn require_and_read_resolve_next_to_the_caller() {
        let t = mem(&[
            ("mod.lua", r#"return { tasks = require("tasks"), n = 1 }"#),
            (
                "tasks/mod.lua",
                r#"return { review = { instructions = read("review.md") }, sub = require("sub") }"#,
            ),
            ("tasks/review.md", "Be kind.\n\n"),
            ("tasks/sub.lua", r#"return { ok = true }"#),
        ]);
        let v = eval(&t, "mod.lua").unwrap();
        assert_eq!(v["tasks"]["review"]["instructions"], "Be kind.");
        assert_eq!(v["tasks"]["sub"]["ok"], true);
        assert_eq!(v["n"], 1);
    }

    #[test]
    fn missing_files_and_escapes_are_errors() {
        let t = mem(&[("mod.lua", r#"return { x = require("nope") }"#)]);
        let e = eval(&t, "mod.lua").unwrap_err().to_string();
        assert!(e.contains("require(\"nope\")"), "{e}");
        let t = mem(&[("mod.lua", r#"return { x = read("../etc/passwd") }"#)]);
        let e = eval(&t, "mod.lua").unwrap_err().to_string();
        assert!(e.contains("may not leave"), "{e}");
        let t = mem(&[("mod.lua", "return { x = io.open('x') }")]);
        assert!(eval(&t, "mod.lua").unwrap_err().to_string().contains("io"));
    }

    #[test]
    fn shipped_tree_evaluates() {
        let v = eval(&Tree::Embedded(super::super::shipped::FILES), "mod.lua").unwrap();
        assert_eq!(v["defaults"]["agent"], "opencode");
        assert!(v["defaults"]["system"].as_str().unwrap().contains("{slug}"));
        assert_eq!(v["agents"]["opencode"]["harness"], "opencode");
        assert!(v["tasks"]["review"]["instructions"]
            .as_str()
            .unwrap()
            .contains("root cause"));
        assert_eq!(v["keys"]["leader"], "Space");
    }
}
