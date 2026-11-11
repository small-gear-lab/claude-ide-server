//! What an adapter's notifications turn into for Claude Code (the middle layer).
//!
//! Like the table of tools, in one place, and a pure function: it decides, and the workspace carries it out.

use serde_json::Value;

use crate::model::{AtMention, ModelError, Selection};

/// What to do with a notification from an adapter.
#[derive(Debug, PartialEq)]
pub(crate) enum Forward {
    /// Nothing is passed on to Claude Code (the notification is for the owner of the workspace only).
    Nothing,
    /// Send this to every connected Claude Code.
    Notify {
        method: &'static str,
        params: Value,
        /// Also send it to a Claude Code that connects later (the last one of its method).
        remember: bool,
    },
    /// The notification is not what it should be, so it is not passed on.
    Malformed(ModelError),
}

/// The decision for a notification: its method, and what the adapter sent.
pub(crate) fn forward(method: &str, params: &Value) -> Forward {
    match method {
        // Kept, so that a Claude Code that connects later starts from the selection that is current.
        "selectionChanged" => match Selection::from_adapter(params) {
            Ok(selection) => Forward::Notify {
                method: "selection_changed",
                params: selection.to_notification(),
                remember: true,
            },
            Err(error) => Forward::Malformed(error),
        },
        "atMention" => match AtMention::from_adapter(params) {
            Ok(mention) => Forward::Notify {
                method: "at_mentioned",
                params: mention.to_notification(),
                remember: false,
            },
            Err(error) => Forward::Malformed(error),
        },
        _ => Forward::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_selection_is_passed_on_and_remembered() {
        let params = json!({
            "filePath": "/a.lua", "text": "x",
            "selection": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
        });
        match forward("selectionChanged", &params) {
            Forward::Notify {
                method,
                params,
                remember,
            } => {
                assert_eq!(method, "selection_changed");
                assert_eq!(params["fileUrl"], "file:///a.lua");
                assert!(remember);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_mention_is_passed_on_and_not_remembered() {
        let params = json!({"filePath": "/a.lua", "lineStart": 1, "lineEnd": 2});
        match forward("atMention", &params) {
            Forward::Notify {
                method, remember, ..
            } => {
                assert_eq!(method, "at_mentioned");
                assert!(!remember);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_malformed_one_is_not_passed_on_and_other_methods_are_left_alone() {
        assert!(matches!(
            forward("selectionChanged", &json!({})),
            Forward::Malformed(_)
        ));
        assert!(matches!(
            forward("atMention", &json!([])),
            Forward::Malformed(_)
        ));
        assert_eq!(
            forward("workspaceFoldersChanged", &json!({})),
            Forward::Nothing
        );
        assert_eq!(forward("somethingNew", &json!(null)), Forward::Nothing);
    }
}
