//! The comparison of a value with a shape, which the other tests rely on.

use claude_ide_test_support as common;

use common::shape::matches_shape;
use serde_json::json;

#[test]
fn types_and_literals_are_compared() {
    let shape = json!({"a": "<string>", "b": "<number>", "c": "<bool>", "d": "<object>", "e": "<array>", "f": "ws", "g": 7, "h": "<any>"});
    let good =
        json!({"a": "x", "b": 1.5, "c": false, "d": {}, "e": [], "f": "ws", "g": 7, "h": null});
    assert_eq!(matches_shape(&good, &shape, "$"), Ok(()));

    for (key, wrong) in [
        ("a", json!(1)),
        ("b", json!("1")),
        ("c", json!(0)),
        ("d", json!([])),
        ("e", json!({})),
        ("f", json!("wss")),
        ("g", json!(8)),
    ] {
        let mut value = good.clone();
        value[key] = wrong;
        let error = matches_shape(&value, &shape, "$").unwrap_err();
        assert!(error.starts_with(&format!("$.{key}:")), "{error}");
    }
}

#[test]
fn a_missing_key_is_reported_and_an_extra_key_is_allowed() {
    let shape = json!({"a": "<string>"});
    assert_eq!(
        matches_shape(&json!({"a": "x", "more": 1}), &shape, "$"),
        Ok(())
    );
    let error = matches_shape(&json!({"b": "x"}), &shape, "$").unwrap_err();
    assert!(error.contains("\"a\" is missing"), "{error}");
}

#[test]
fn a_one_element_array_shape_applies_to_every_element_with_its_path() {
    let shape = json!({"items": [ {"id": "<number>"} ]});
    assert_eq!(matches_shape(&json!({"items": []}), &shape, "$"), Ok(()));
    assert_eq!(
        matches_shape(&json!({"items": [{"id": 1}, {"id": 2}]}), &shape, "$"),
        Ok(())
    );
    let error =
        matches_shape(&json!({"items": [{"id": 1}, {"id": "x"}]}), &shape, "$").unwrap_err();
    assert!(error.starts_with("$.items[1].id:"), "{error}");
}

#[test]
fn a_shape_array_with_other_than_one_element_is_refused() {
    let error = matches_shape(&json!([1]), &json!(["<number>", "<number>"]), "$").unwrap_err();
    assert!(error.contains("exactly one element"), "{error}");
}
