// Assertions for tests: JSON diffs, golden snapshots and a JSON Schema subset for OpenAPI outputs.
use crate::json::Json;
use std::path::PathBuf;

// Asserts two JSON values are equal, listing the paths that differ.
#[macro_export]
macro_rules! assert_json_eq {
    ($left:expr, $right:expr $(,)?) => {{
        let diffs = $crate::test::json_diff(&$crate::Json::from($left), &$crate::Json::from($right));
        assert!(diffs.is_empty(), "JSON differs:\n{}", diffs.join("\n"));
    }};
}

// Lists the paths where two values differ, with both sides.
pub fn json_diff(a: &Json, b: &Json) -> Vec<String> {
    let mut out = Vec::new();
    diff_at("$", a, b, &mut out);
    out
}

// Compares two values at a path, recursing into arrays and objects.
fn diff_at(path: &str, a: &Json, b: &Json, out: &mut Vec<String>) {
    match (a, b) {
        (Json::Obj(x), Json::Obj(y)) => {
            let mut keys: Vec<&String> = x.iter().map(|(k, _)| k).collect();
            keys.extend(y.iter().map(|(k, _)| k).filter(|k| !x.iter().any(|(xk, _)| xk == *k)));
            for k in keys {
                diff_at(&format!("{path}.{k}"), &a[k.as_str()], &b[k.as_str()], out);
            }
        }
        (Json::Arr(x), Json::Arr(y)) => {
            for i in 0..x.len().max(y.len()) {
                diff_at(&format!("{path}[{i}]"), &a[i], &b[i], out);
            }
        }
        _ if a.f64().is_some() && a.f64() == b.f64() => {}
        _ if a != b => out.push(format!("{path}: {a} != {b}")),
        _ => {}
    }
}

// The snapshot directory: $ZAZZ_SNAPSHOTS, or tests/snapshots.
fn snapshot_dir() -> PathBuf {
    std::env::var("ZAZZ_SNAPSHOTS").map_or_else(|_| PathBuf::from("tests/snapshots"), PathBuf::from)
}

// Compares a value with tests/snapshots/NAME.json; with ZAZZ_UPDATE_SNAPSHOTS=1 it writes the file instead.
pub fn assert_snapshot(name: &str, value: &Json) {
    let path = snapshot_dir().join(format!("{name}.json"));
    let update = std::env::var("ZAZZ_UPDATE_SNAPSHOTS").is_ok_and(|v| v == "1");
    let current = std::fs::read_to_string(&path).ok();
    if update && current.as_deref() != Some(&(value.pretty() + "\n")) {
        std::fs::create_dir_all(path.parent().expect("snapshot files have a parent"))
            .expect("create snapshot dir");
        std::fs::write(&path, value.pretty() + "\n").expect("write snapshot");
        return;
    }
    let Some(text) = current else {
        panic!(
            "zazz::test: snapshot {} is missing; run `zazz test DIR --update` to create it",
            path.display()
        );
    };
    let want = Json::parse(&text)
        .unwrap_or_else(|e| panic!("zazz::test: snapshot {} is not JSON: {e}", path.display()));
    let diffs = json_diff(value, &want);
    assert!(diffs.is_empty(), "snapshot {name} differs (got != want):\n{}", diffs.join("\n"));
}

// Resolves a local "$ref" like "#/components/schemas/Item" against the whole document.
fn resolve<'a>(schema: &'a Json, doc: &'a Json) -> &'a Json {
    let Some(r) = schema["$ref"].str().and_then(|r| r.strip_prefix("#/")) else { return schema };
    r.split('/').fold(doc, |node, key| &node[key])
}

// True when a value has one JSON Schema type name.
fn has_type(v: &Json, ty: &str) -> bool {
    match ty {
        "null" => v.is_null(),
        "boolean" => v.bool().is_some(),
        "integer" => matches!(v, Json::Int(_)) || v.i64().is_some() && v.f64().is_some(),
        "number" => v.f64().is_some(),
        "string" => v.str().is_some(),
        "array" => v.arr().is_some(),
        "object" => v.obj().is_some(),
        _ => true,
    }
}

// Checks a value against a schema subset (type, enum, required, properties, additionalProperties: false, items).
pub fn check_schema(v: &Json, schema: &Json, doc: &Json) -> Vec<String> {
    let mut out = Vec::new();
    check_at("$", v, schema, doc, &mut out);
    out
}

// Checks one value at a path.
fn check_at(path: &str, v: &Json, schema: &Json, doc: &Json, out: &mut Vec<String>) {
    let s = resolve(schema, doc);
    let types: Vec<&str> = match &s["type"] {
        Json::Str(t) => vec![t.as_str()],
        Json::Arr(ts) => ts.iter().filter_map(Json::str).collect(),
        _ => vec![],
    };
    if !types.is_empty() && !types.iter().any(|t| has_type(v, t)) {
        out.push(format!("{path}: expected {}, got {v}", types.join(" or ")));
        return;
    }
    if let Some(choices) = s["enum"].arr() {
        if !choices.contains(v) {
            out.push(format!("{path}: {v} is not one of the allowed values"));
        }
    }
    if let Some(fields) = v.obj() {
        for req in s["required"].arr().unwrap_or(&[]).iter().filter_map(Json::str) {
            if v.get(req).is_none() {
                out.push(format!("{path}: missing required field '{req}'"));
            }
        }
        let props = &s["properties"];
        for (k, fv) in fields {
            match props.get(k) {
                Some(ps) => check_at(&format!("{path}.{k}"), fv, ps, doc, out),
                None if s["additionalProperties"] == Json::Bool(false) => {
                    out.push(format!("{path}: unexpected field '{k}'"))
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(schema)) = (v.arr(), s.get("items")) {
        for (i, item) in items.iter().enumerate() {
            check_at(&format!("{path}[{i}]"), item, schema, doc, out);
        }
    }
}

// Finds the JSON response schema of an operation in an OpenAPI document.
pub fn response_schema<'a>(doc: &'a Json, operation: &str) -> Option<&'a Json> {
    for (_, item) in doc["paths"].obj().unwrap_or(&[]) {
        for (_, op) in item.obj().unwrap_or(&[]) {
            if op["operationId"].str() != Some(operation) {
                continue;
            }
            let responses = &op["responses"];
            let ok =
                responses.get("200").or_else(|| responses.get("201")).or_else(|| responses.get("default"))?;
            return ok["content"]["application/json"].get("schema");
        }
    }
    None
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::json;

    // Checks diffs report every differing path and treat 1 and 1.0 as equal.
    #[test]
    fn test_json_diff() {
        let a = json!({"a": 1, "b": [1, 2], "c": {"d": "x"}, "only_a": true});
        let b = json!({"a": 1.0, "b": [1], "c": {"d": "y"}, "only_b": null});
        assert_eq!(
            json_diff(&a, &b),
            vec!["$.b[1]: 2 != null", "$.c.d: \"x\" != \"y\"", "$.only_a: true != null"]
        );
        assert!(json_diff(&a, &a).is_empty());
        assert_json_eq!(json!([1, 2]), json!([1, 2]));
    }

    // Checks the assertion macro fails with the diff.
    #[test]
    #[should_panic(expected = "$.a: 1 != 2")]
    fn test_assert_json_eq_fails() {
        assert_json_eq!(json!({"a": 1}), json!({"a": 2}));
    }

    // Checks the schema subset: types, enums, required, properties, extras, items and refs.
    #[test]
    fn test_check_schema() {
        let doc = json!({"components": {"schemas": {"Row": {"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}, "additionalProperties": false}}}});
        let schema = json!({"type": "object", "properties": {
            "rows": {"type": "array", "items": {"$ref": "#/components/schemas/Row"}},
            "kind": {"type": "string", "enum": ["a", "b"]},
            "opt": {"type": ["string", "null"]},
            "f": {"type": "number"}, "b": {"type": "boolean"}, "any": {}
        }});
        let good = json!({"rows": [{"n": 1}, {"n": 2.0}], "kind": "a", "opt": null, "f": 1.5, "b": true, "any": [1]});
        assert_eq!(check_schema(&good, &schema, &doc), Vec::<String>::new());
        let bad =
            json!({"rows": [{"n": "x"}, {}, {"n": 1, "z": 1}], "kind": "c", "opt": 3, "f": "x", "b": 1});
        let errs = check_schema(&bad, &schema, &doc);
        assert_eq!(errs.len(), 7, "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("missing required field 'n'")));
        assert!(errs.iter().any(|e| e.contains("unexpected field 'z'")));
        assert!(errs.iter().any(|e| e.contains("not one of")));
        assert_eq!(check_schema(&json!(1), &json!({"type": "null"}), &doc).len(), 1);
        assert!(check_schema(&json!(1), &json!({"type": "weird"}), &doc).is_empty());
    }

    // Checks operations are found by operationId with 200, 201 or default responses.
    #[test]
    fn test_response_schema() {
        let doc = json!({"paths": {"/a": {
            "post": {"operationId": "make", "responses": {"201": {"content": {"application/json": {"schema": {"type": "object"}}}}}},
            "get": {"operationId": "list", "responses": {"default": {"content": {"application/json": {"schema": {"type": "array"}}}}}},
            "put": {"operationId": "none", "responses": {}}
        }}});
        assert_eq!(response_schema(&doc, "make").expect("make")["type"].str(), Some("object"));
        assert_eq!(response_schema(&doc, "list").expect("list")["type"].str(), Some("array"));
        assert!(response_schema(&doc, "none").is_none());
        assert!(response_schema(&doc, "missing").is_none());
    }

    // Checks snapshots: missing fails, update writes, then matching passes and differing fails.
    #[test]
    fn test_snapshots() {
        let dir = std::env::temp_dir().join(format!("zazz-snap-{}", std::process::id()));
        let _guard = crate::test::env_lock();
        std::env::set_var("ZAZZ_SNAPSHOTS", &dir);
        let missing = std::panic::catch_unwind(|| assert_snapshot("s", &json!({"a": 1})));
        assert!(missing.is_err());
        std::env::set_var("ZAZZ_UPDATE_SNAPSHOTS", "1");
        assert_snapshot("s", &json!({"a": 1}));
        assert_snapshot("s", &json!({"a": 1}));
        std::env::remove_var("ZAZZ_UPDATE_SNAPSHOTS");
        assert_snapshot("s", &json!({"a": 1}));
        assert!(std::panic::catch_unwind(|| assert_snapshot("s", &json!({"a": 2}))).is_err());
        std::fs::write(dir.join("bad.json"), "{").expect("bad");
        assert!(std::panic::catch_unwind(|| assert_snapshot("bad", &json!(1))).is_err());
        std::env::remove_var("ZAZZ_SNAPSHOTS");
        assert_eq!(snapshot_dir(), PathBuf::from("tests/snapshots"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
