//! The tools that Claude Code can call, and what each one does in a workspace (the middle layer).
//!
//! A tool is backed either by a method of the adapter, which the adapter must have declared in `register`, or by the
//! server itself. Only the tools that can be run are listed. The names, the arguments and the results follow what the
//! existing IDE integrations offer, which is what Claude Code expects.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::adapter::{AdapterHandle, RequestError};
use crate::mcp::{ToolBackend, ToolDefinition, ToolError, ToolResult};
use crate::model::{
    self, ModelError, SelectionAnswer, diagnostics_to_tool_text, file_uri,
    open_editors_to_tool_text,
};

/// What runs a tool.
enum Backing {
    /// A method of the adapter, by this name.
    Adapter(&'static str),
    /// The server answers it from what it knows.
    Server,
}

/// How the answer of the adapter becomes the text that Claude gets.
#[derive(Clone, Copy)]
enum Answer {
    /// Passed on as it is (a string), or written as JSON.
    Raw,
    /// Checked and rewritten through the types of [`model`](crate::model), so that a malformed answer is a failed call.
    Selection,
    OpenEditors,
    Diagnostics,
}

struct ToolSpec {
    name: &'static str,
    backing: Backing,
    answer: Answer,
    description: &'static str,
    schema: fn() -> Value,
}

fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {} })
}

fn file_path_only() -> Value {
    json!({
        "type": "object",
        "properties": { "filePath": { "type": "string", "description": "The absolute path of the file." } },
        "required": ["filePath"],
    })
}

fn open_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "filePath": { "type": "string", "description": "The path of the file to open." },
            "preview": { "type": "boolean", "description": "Open it as a preview, not as a permanent tab.", "default": false },
            "startText": { "type": "string", "description": "Select from the first text that matches this." },
            "endText": { "type": "string", "description": "Select up to the first text that matches this." },
            "selectToEndOfLine": { "type": "boolean", "description": "Extend the selection to the end of its last line.", "default": false },
            "makeFrontmost": { "type": "boolean", "description": "Make the file the active one.", "default": true },
        },
        "required": ["filePath"],
    })
}

fn open_diff_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "old_file_path": { "type": "string", "description": "The path of the file as it is." },
            "new_file_path": { "type": "string", "description": "The path of the file that the change is for." },
            "new_file_contents": { "type": "string", "description": "What the file would hold after the change." },
            "tab_name": { "type": "string", "description": "The name to show the diff under." },
        },
        "required": ["old_file_path", "new_file_path", "new_file_contents", "tab_name"],
    })
}

fn close_tab_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "tab_name": { "type": "string", "description": "The tab_name that was given to openDiff." } },
        "required": ["tab_name"],
    })
}

fn get_diagnostics_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "uri": { "type": "string", "description": "A file URI; all files if it is left out." } },
    })
}

const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "openFile",
        backing: Backing::Adapter("openFile"),
        answer: Answer::Raw,
        description: "Open a file in the editor and optionally select a range of text.",
        schema: open_file_schema,
    },
    ToolSpec {
        name: "openDiff",
        backing: Backing::Adapter("openDiff"),
        answer: Answer::Raw,
        description: "Show a proposed change to a file as a diff and wait until the user accepts or rejects it.",
        schema: open_diff_schema,
    },
    ToolSpec {
        name: "close_tab",
        backing: Backing::Adapter("closeDiff"),
        answer: Answer::Raw,
        description: "Close a diff that openDiff opened, by its tab_name.",
        schema: close_tab_schema,
    },
    ToolSpec {
        name: "closeAllDiffTabs",
        backing: Backing::Adapter("closeAllDiffs"),
        answer: Answer::Raw,
        description: "Close all the diffs that openDiff opened.",
        schema: no_arguments,
    },
    ToolSpec {
        name: "getOpenEditors",
        backing: Backing::Adapter("getOpenEditors"),
        answer: Answer::OpenEditors,
        description: "List the files that are open in the editor.",
        schema: no_arguments,
    },
    ToolSpec {
        name: "getCurrentSelection",
        backing: Backing::Adapter("getCurrentSelection"),
        answer: Answer::Selection,
        description: "Get the selection in the active editor.",
        schema: no_arguments,
    },
    ToolSpec {
        name: "getLatestSelection",
        backing: Backing::Adapter("getLatestSelection"),
        answer: Answer::Selection,
        description: "Get the most recent selection, even if the editor has lost focus.",
        schema: no_arguments,
    },
    ToolSpec {
        name: "getWorkspaceFolders",
        backing: Backing::Server,
        answer: Answer::Raw,
        description: "Get the folders of the workspace that the editor has open.",
        schema: no_arguments,
    },
    ToolSpec {
        name: "getDiagnostics",
        backing: Backing::Adapter("getDiagnostics"),
        answer: Answer::Diagnostics,
        description: "Get the diagnostics (errors and warnings) of one file or of all files.",
        schema: get_diagnostics_schema,
    },
    ToolSpec {
        name: "checkDocumentDirty",
        backing: Backing::Adapter("checkDocumentDirty"),
        answer: Answer::Raw,
        description: "Check whether a file has unsaved changes.",
        schema: file_path_only,
    },
    ToolSpec {
        name: "saveDocument",
        backing: Backing::Adapter("saveDocument"),
        answer: Answer::Raw,
        description: "Save a file that has unsaved changes.",
        schema: file_path_only,
    },
];

/// The tools of one workspace.
pub(crate) struct WorkspaceTools {
    /// The methods that the adapter declared.
    methods: Vec<String>,
    adapter: AdapterHandle,
    /// The folders of the workspace as they are now.
    folders: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl WorkspaceTools {
    pub(crate) fn new(
        methods: Vec<String>,
        adapter: AdapterHandle,
        folders: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            methods,
            adapter,
            folders,
        }
    }

    fn can_run(&self, spec: &ToolSpec) -> bool {
        match spec.backing {
            Backing::Server => true,
            Backing::Adapter(method) => self.methods.iter().any(|declared| declared == method),
        }
    }
}

impl ToolBackend for WorkspaceTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        TOOLS
            .iter()
            .filter(|spec| self.can_run(spec))
            .map(|spec| ToolDefinition {
                name: spec.name.to_owned(),
                description: spec.description.to_owned(),
                input_schema: (spec.schema)(),
            })
            .collect()
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult, ToolError> {
        let spec = TOOLS
            .iter()
            .find(|spec| spec.name == name && self.can_run(spec))
            .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?;
        Ok(match spec.backing {
            Backing::Server => ToolResult {
                text: workspace_folders_text(&(self.folders)()),
                is_error: false,
            },
            Backing::Adapter(method) => match self.adapter.request(method, arguments).await {
                Ok(result) => match answer_text(spec.answer, &result) {
                    Ok(text) => ToolResult {
                        text,
                        is_error: false,
                    },
                    Err(error) => ToolResult {
                        text: format!("the editor's answer to {name} is not valid: {error}"),
                        is_error: true,
                    },
                },
                Err(error) => ToolResult {
                    text: error_text(&error),
                    is_error: true,
                },
            },
        })
    }
}

fn answer_text(answer: Answer, result: &Value) -> Result<String, ModelError> {
    Ok(match answer {
        Answer::Raw => result_text(result),
        Answer::Selection => SelectionAnswer::from_adapter(result)?.to_tool_text(),
        Answer::OpenEditors => {
            open_editors_to_tool_text(&model::open_editors_from_adapter(result)?)
        }
        Answer::Diagnostics => diagnostics_to_tool_text(&model::diagnostics_from_adapter(result)?),
    })
}

/// A string is the text as it is (for example `FILE_SAVED`); anything else is written as JSON.
fn result_text(result: &Value) -> String {
    match result {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn error_text(error: &RequestError) -> String {
    match error {
        RequestError::Rpc(error) => error.message.clone(),
        RequestError::Timeout => "the editor did not answer in time".to_owned(),
        RequestError::Disconnected => "the editor is not connected".to_owned(),
        RequestError::NotDeclared(method) => format!("the editor does not implement {method}"),
    }
}

fn workspace_folders_text(folders: &[String]) -> String {
    let entries: Vec<Value> = folders
        .iter()
        .map(|path| {
            json!({
                "name": path.trim_end_matches('/').rsplit('/').next().unwrap_or(path),
                "uri": file_uri(path),
                "path": path,
            })
        })
        .collect();
    json!({
        "success": true,
        "folders": entries,
        "rootPath": folders.first(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_are_described_with_a_name_a_uri_and_a_path() {
        let text = workspace_folders_text(&["/home/u/app".to_owned(), "/srv/www/".to_owned()]);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["success"], true);
        assert_eq!(value["rootPath"], "/home/u/app");
        assert_eq!(value["folders"][0]["name"], "app");
        assert_eq!(value["folders"][0]["uri"], "file:///home/u/app");
        assert_eq!(value["folders"][1]["name"], "www");
    }

    #[test]
    fn a_string_result_is_used_as_it_is_and_anything_else_as_json() {
        assert_eq!(result_text(&json!("FILE_SAVED")), "FILE_SAVED");
        assert_eq!(result_text(&json!({"a": 1})), r#"{"a":1}"#);
    }

    #[test]
    fn every_tool_has_an_object_schema() {
        for spec in TOOLS {
            assert_eq!((spec.schema)()["type"], "object", "{}", spec.name);
        }
    }
}
