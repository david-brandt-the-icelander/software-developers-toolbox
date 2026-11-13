// zazz ls, rm and describe: look at and remove registered scripts.
use super::pipeline::err;
use super::Zazz;
use serde_json::{json, Value};

// A one-line test summary from a stored report.
fn tests_summary(t: &Value) -> Value {
    if t.is_null() {
        return json!("none");
    }
    json!(format!(
        "{} passed, {} ignored",
        t["passed"].as_i64().unwrap_or(0),
        t["ignored"].as_i64().unwrap_or(0)
    ))
}

// Runs `zazz ls`: every registered script, newest first.
pub fn ls(z: &Zazz) -> Result<Value, Value> {
    let reg = z.registry().map_err(|e| err("store", &e))?;
    let mut rows: Vec<Value> = reg
        .ids()
        .iter()
        .filter_map(|hex| reg.read_meta(hex))
        .map(|m| {
            let id = m["id"].as_str().unwrap_or("").chars().take(15).collect::<String>();
            json!({"id": id, "name": m["name"], "version": m["version"], "runtime": m["runtime"], "tests": tests_summary(&m["tests"]), "added_at": m["added_at"]})
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r["added_at"].as_u64().unwrap_or(0)));
    Ok(Value::Array(rows))
}

// Runs `zazz rm ID`.
pub fn rm(z: &Zazz, args: &[String]) -> Result<Value, Value> {
    let [what] = args else { return Err(err("usage", "usage: zazz rm ID|PREFIX|NAME")) };
    let reg = z.registry().map_err(|e| err("store", &e))?;
    let hex = reg.resolve(what).map_err(|e| err("not_found", &e))?;
    reg.remove(&hex);
    Ok(json!({"removed": format!("b3:{hex}")}))
}

// Lists the operation ids in an OpenAPI document.
fn operations(doc: &Value) -> Vec<Value> {
    let Some(paths) = doc["paths"].as_object() else { return Vec::new() };
    paths
        .values()
        .filter_map(Value::as_object)
        .flat_map(|ops| ops.values())
        .filter_map(|op| op.get("operationId").cloned())
        .collect()
}

// Runs `zazz describe ID`: what the script is, how it runs and what it may touch.
pub fn describe(z: &Zazz, args: &[String]) -> Result<Value, Value> {
    let [what] = args else { return Err(err("usage", "usage: zazz describe ID|PREFIX|NAME")) };
    let reg = z.registry().map_err(|e| err("store", &e))?;
    let hex = reg.resolve(what).map_err(|e| err("not_found", &e))?;
    let m = reg.read_meta(&hex).ok_or_else(|| err("not_found", &format!("no metadata for b3:{hex}")))?;
    let man = &m["manifest"];
    Ok(json!({
        "id": m["id"],
        "name": m["name"],
        "version": m["version"],
        "description": m["description"],
        "runtime": m["runtime"],
        "messages": m["messages"],
        "sandbox": man.get("sandbox").cloned().unwrap_or(json!({"read": ["$CWD"]})),
        "cli": man.get("cli").is_some(),
        "operations": operations(&m["openapi"]),
        "tests": tests_summary(&m["tests"]),
        "signer": m["signer"],
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks the summaries and operation listing.
    #[test]
    fn test_helpers() {
        assert_eq!(tests_summary(&Value::Null), json!("none"));
        assert_eq!(tests_summary(&json!({"passed": 2})), json!("2 passed, 0 ignored"));
        let doc = json!({"paths": {"/a": {"get": {"operationId": "list"}, "post": {}}, "/b": 1}});
        assert_eq!(operations(&doc), vec![json!("list")]);
        assert!(operations(&Value::Null).is_empty());
    }
}
