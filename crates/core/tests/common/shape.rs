//! Fixtures, and the comparison of a value with a shape (see `tests/fixtures/README.md`).

use std::path::PathBuf;

use serde_json::Value;

/// Reads `tests/fixtures/<name>`.
pub fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Checks `actual` against the shape in `tests/fixtures/<shape_name>`, and panics with the path of the first
/// difference.
pub fn assert_shape(actual: &Value, shape_name: &str) {
    if let Err(difference) = matches_shape(actual, &fixture(shape_name), "$") {
        panic!("{shape_name}: {difference}\nactual: {actual}");
    }
}

/// The first difference between `actual` and `shape`, as a path and a reason.
pub fn matches_shape(actual: &Value, shape: &Value, path: &str) -> Result<(), String> {
    match shape {
        Value::String(expected) => match expected.as_str() {
            "<string>" => expect(actual.is_string(), path, "a string"),
            "<number>" => expect(actual.is_number(), path, "a number"),
            "<bool>" => expect(actual.is_boolean(), path, "a boolean"),
            "<object>" => expect(actual.is_object(), path, "an object"),
            "<array>" => expect(actual.is_array(), path, "an array"),
            "<any>" => Ok(()),
            literal => expect(
                actual.as_str() == Some(literal),
                path,
                &format!("the string {literal:?}"),
            ),
        },
        Value::Object(keys) => {
            let Some(object) = actual.as_object() else {
                return Err(format!("{path}: expected an object"));
            };
            for (key, sub_shape) in keys {
                let Some(value) = object.get(key) else {
                    return Err(format!("{path}: the key {key:?} is missing"));
                };
                matches_shape(value, sub_shape, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        Value::Array(items) => {
            let [element_shape] = items.as_slice() else {
                return Err(format!("{path}: a shape array has exactly one element"));
            };
            let Some(elements) = actual.as_array() else {
                return Err(format!("{path}: expected an array"));
            };
            for (index, element) in elements.iter().enumerate() {
                matches_shape(element, element_shape, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        literal => expect(actual == literal, path, &format!("the value {literal}")),
    }
}

fn expect(holds: bool, path: &str, what: &str) -> Result<(), String> {
    if holds {
        Ok(())
    } else {
        Err(format!("{path}: expected {what}"))
    }
}
