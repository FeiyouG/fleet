//! Deep merge of two config trees (JSON values): the user's file over the shipped one.
//!
//! Objects merge key by key, recursively. Anything else (scalars, arrays) in `over` replaces
//! the base value: `skills = { "a" }` replaces the whole list, it does not append.

use serde_json::Value;

pub fn deep_merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(bv) => deep_merge(bv, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Object(b)
        }
        (_, over) => over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn objects_merge_scalars_and_lists_replace() {
        let base = json!({"defaults": {"agent": "opencode", "mode": "plan"},
                          "tasks": {"start": {"key": "s", "skills": ["a"]}, "review": {"key": "r"}}});
        let over = json!({"defaults": {"mode": "build"},
                          "tasks": {"start": {"skills": ["b", "c"]}, "bugfix": {"key": "b"}}});
        let m = deep_merge(base, over);
        assert_eq!(m["defaults"]["agent"], "opencode");
        assert_eq!(m["defaults"]["mode"], "build");
        assert_eq!(m["tasks"]["start"]["key"], "s");
        assert_eq!(m["tasks"]["start"]["skills"], json!(["b", "c"]));
        assert_eq!(m["tasks"]["review"]["key"], "r");
        assert_eq!(m["tasks"]["bugfix"]["key"], "b");
    }

    #[test]
    fn non_object_over_object_replaces() {
        assert_eq!(deep_merge(json!({"a": 1}), json!(5)), json!(5));
        assert_eq!(deep_merge(json!([1, 2]), json!([3])), json!([3]));
    }
}
