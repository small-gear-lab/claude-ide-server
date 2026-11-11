//! The tools that Claude Code can call, and what each one does in a workspace (the middle layer).
//!
//! A tool is backed either by a method of the adapter, which the adapter must have declared in `register`, or by the
//! server itself. Only the tools that can be run are listed. The names, the arguments and the results follow what the
//! existing IDE integrations offer, which is what Claude Code expects.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::adapter::{AdapterHandle, RequestError};
use crate::mcp::{ToolBackend, ToolDefinition, ToolError, ToolResult};
use crate::model::{
    self, DiffOutcome, ModelError, OpenFileRequest, OpenedFile, SelectionAnswer,
    closed_diffs_from_adapter, closed_diffs_to_tool_text, diagnostics_to_tool_text,
    document_state_from_adapter, document_state_to_tool_text, file_uri, open_editors_to_tool_text,
    save_from_adapter, save_to_tool_text,
};
use crate::paths;

/// What runs a tool.
enum Backing {
    /// A method of the adapter, by this name.
    Adapter(&'static str),
    /// A method of the adapter if it has one, and otherwise this answer. For what Claude Code asks for without being
    /// told that it can: it calls `closeAllDiffTabs` right after it connects, whether or not the tool was listed
    /// (observed with 2.1.322), and an adapter that shows no diffs has none to close.
    AdapterOrElse(&'static str, &'static str),
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
    /// As `Selection`, for `getLatestSelection`, whose result has no `success`.
    LatestSelection,
    OpenEditors,
    Diagnostics,
    OpenFile,
    DocumentDirty,
    SaveDocument,
    /// `close_tab` says `TAB_CLOSED` whatever the adapter answers (the extension does too, even if there was no such tab).
    TabClosed,
    ClosedDiffs,
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

fn set_permission_mode_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "mode": { "type": "string", "description": "`ask`, or `skip_all_permission_checks`." } },
        "required": ["mode"],
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
        answer: Answer::OpenFile,
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
        backing: Backing::AdapterOrElse("closeDiff", "TAB_CLOSED"),
        answer: Answer::TabClosed,
        description: "Close a diff that openDiff opened, by its tab_name.",
        schema: close_tab_schema,
    },
    ToolSpec {
        name: "closeAllDiffTabs",
        backing: Backing::AdapterOrElse("closeAllDiffs", "CLOSED_0_DIFF_TABS"),
        answer: Answer::ClosedDiffs,
        description: "Close all the diffs that openDiff opened.",
        schema: no_arguments,
    },
    ToolSpec {
        name: "set_permission_mode",
        // Claude Code tells the editor how it asks for permission, whenever that changes (seen in the code of 2.1.322:
        // `ask`, or `skip_all_permission_checks`). An editor that has no use for it need not say so.
        backing: Backing::AdapterOrElse("setPermissionMode", "OK"),
        answer: Answer::Raw,
        description: "Tell the editor how Claude Code asks for permission.",
        schema: set_permission_mode_schema,
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
        answer: Answer::LatestSelection,
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
        answer: Answer::DocumentDirty,
        description: "Check whether a file has unsaved changes.",
        schema: file_path_only,
    },
    ToolSpec {
        name: "saveDocument",
        backing: Backing::Adapter("saveDocument"),
        answer: Answer::SaveDocument,
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
    /// The number of the next diff that is opened: it tells apart two diffs of the same name.
    next_diff: AtomicU64,
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
            next_diff: AtomicU64::new(1),
        }
    }

    fn declared(&self, method: &str) -> bool {
        self.methods.iter().any(|declared| declared == method)
    }

    /// Whether the tool is offered to Claude: the adapter can answer it, or the server does.
    fn listed(&self, spec: &ToolSpec) -> bool {
        match spec.backing {
            Backing::Server => true,
            Backing::Adapter(method) | Backing::AdapterOrElse(method, _) => self.declared(method),
        }
    }

    /// Whether the tool may be called. Some are called although they were not listed.
    fn callable(&self, spec: &ToolSpec) -> bool {
        matches!(spec.backing, Backing::AdapterOrElse(..)) || self.listed(spec)
    }
}

/// Closes a diff that was opened for a call that did not end with an answer (Claude Code cancelled it, or went away),
/// so that it does not stay on the screen of the editor with nobody to ask. It names the diff by its number, so that it
/// cannot close a newer diff of the same name.
struct CloseOnDrop {
    adapter: AdapterHandle,
    tab_name: String,
    id: u64,
    armed: bool,
}

impl CloseOnDrop {
    fn defuse(&mut self) {
        self.armed = false;
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (adapter, tab_name, id) = (self.adapter.clone(), self.tab_name.clone(), self.id);
        runtime.spawn(async move {
            // The adapter may not have `closeDiff`, or be gone: nothing is left to do then.
            let _ = adapter
                .request("closeDiff", json!({ "tabName": tab_name, "diffId": id }))
                .await;
        });
    }
}

impl WorkspaceTools {
    /// `openDiff`: the adapter shows the diff and answers when the user has decided, which may be a long time, so it is
    /// not asked with a time limit (it ends if the adapter goes away). Nothing else waits for it: other calls go on.
    async fn open_diff(&self, arguments: &Value) -> ToolResult {
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let tab_name = text("tab_name").to_owned();
        let id = self.next_diff.fetch_add(1, Ordering::Relaxed);
        let params = json!({
            "oldFilePath": text("old_file_path"),
            "newFilePath": text("new_file_path"),
            "newFileContents": text("new_file_contents"),
            "tabName": tab_name,
            "diffId": id,
        });
        let mut closing = CloseOnDrop {
            adapter: self.adapter.clone(),
            tab_name: tab_name.clone(),
            id,
            armed: true,
        };
        let answer = self.adapter.request_with("openDiff", params, None).await;
        closing.defuse();
        let failed = |text: String| ToolResult {
            text,
            more: Vec::new(),
            is_error: true,
        };
        match answer {
            Ok(result) => match DiffOutcome::from_adapter(&result) {
                Ok(outcome) => {
                    let (first, more) = outcome.to_tool_texts(&tab_name);
                    ToolResult {
                        text: first,
                        more,
                        is_error: false,
                    }
                }
                Err(error) => failed(format!(
                    "the editor's answer to openDiff is not valid: {error}"
                )),
            },
            Err(error) => failed(error_text(&error)),
        }
    }
}

impl ToolBackend for WorkspaceTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        TOOLS
            .iter()
            .filter(|spec| self.listed(spec))
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
            .find(|spec| spec.name == name && self.callable(spec))
            .ok_or_else(|| ToolError::UnknownTool(name.to_owned()))?;
        // What the model sends is not trusted to match the schema that it was given.
        if let Err(reason) = check_arguments(&(spec.schema)(), &arguments) {
            return Ok(ToolResult {
                text: format!("invalid arguments for {name}: {reason}"),
                more: Vec::new(),
                is_error: true,
            });
        }
        // A path that is not absolute is taken from the first folder of the workspace, as the VS Code extension does, so
        // that the adapter always gets an absolute one.
        let mut arguments = arguments;
        absolute_file_path(&mut arguments, &(self.folders)());
        if spec.name == "openDiff" {
            return Ok(self.open_diff(&arguments).await);
        }
        let method = match spec.backing {
            Backing::Server => {
                return Ok(ToolResult {
                    text: workspace_folders_text(&(self.folders)()),
                    more: Vec::new(),
                    is_error: false,
                });
            }
            Backing::Adapter(method) => method,
            Backing::AdapterOrElse(method, otherwise) => {
                if !self.declared(method) {
                    return Ok(ToolResult {
                        text: otherwise.to_owned(),
                        more: Vec::new(),
                        is_error: false,
                    });
                }
                method
            }
        };
        Ok(
            match self
                .adapter
                .request(method, adapter_arguments(spec.name, &arguments))
                .await
            {
                Ok(result) => match answer_text(spec.answer, &result, &arguments) {
                    Ok(text) => ToolResult {
                        text,
                        more: Vec::new(),
                        is_error: false,
                    },
                    Err(error) => ToolResult {
                        text: format!("the editor's answer to {name} is not valid: {error}"),
                        more: Vec::new(),
                        is_error: true,
                    },
                },
                Err(error) => ToolResult {
                    text: error_text(&error),
                    more: Vec::new(),
                    is_error: true,
                },
            },
        )
    }
}

/// Checks the arguments of a call against the schema of the tool: the required ones are there, and those that are
/// given have the type that the schema says. (The schemas here are flat: a few properties of simple types.)
fn check_arguments(schema: &Value, arguments: &Value) -> Result<(), String> {
    let given = arguments
        .as_object()
        .ok_or_else(|| "the arguments must be an object".to_owned())?;
    for key in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if given.get(key).is_none_or(Value::is_null) {
            return Err(format!("{key} is required"));
        }
    }
    for (key, property) in schema["properties"].as_object().into_iter().flatten() {
        let Some(value) = given.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        let (holds, expected) = match property["type"].as_str() {
            Some("string") => (value.is_string(), "a string"),
            Some("boolean") => (value.is_boolean(), "a boolean"),
            Some("number" | "integer") => (value.is_number(), "a number"),
            Some("object") => (value.is_object(), "an object"),
            Some("array") => (value.is_array(), "an array"),
            _ => (true, ""),
        };
        if !holds {
            return Err(format!("{key} must be {expected}"));
        }
    }
    Ok(())
}

fn answer_text(answer: Answer, result: &Value, arguments: &Value) -> Result<String, ModelError> {
    let file_path = || {
        arguments
            .get("filePath")
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    Ok(match answer {
        Answer::Raw => result_text(result),
        Answer::Selection => SelectionAnswer::from_adapter(result)?.to_tool_text(),
        Answer::LatestSelection => SelectionAnswer::from_adapter(result)?.to_latest_text(),
        Answer::OpenEditors => {
            open_editors_to_tool_text(&model::open_editors_from_adapter(result)?)
        }
        Answer::Diagnostics => diagnostics_to_tool_text(&model::diagnostics_from_adapter(result)?),
        Answer::OpenFile => {
            let request = OpenFileRequest::from_arguments(arguments);
            OpenedFile::from_adapter(result, request.start_text.is_some())?.to_tool_text(&request)
        }
        Answer::DocumentDirty => {
            document_state_to_tool_text(file_path(), document_state_from_adapter(result)?)
        }
        Answer::SaveDocument => save_to_tool_text(file_path(), save_from_adapter(result)?),
        Answer::TabClosed => "TAB_CLOSED".to_owned(),
        Answer::ClosedDiffs => closed_diffs_to_tool_text(closed_diffs_from_adapter(result)?),
    })
}

/// The parameters that the adapter is given for a tool: the arguments of the call, except where the adapter's name for
/// a parameter is not the tool's (`close_tab` takes `tab_name`, as the extension does; the adapter is told `tabName`,
/// as with `openDiff`).
fn adapter_arguments(tool: &str, arguments: &Value) -> Value {
    if tool == "close_tab" {
        return json!({ "tabName": arguments.get("tab_name") });
    }
    arguments.clone()
}

/// Makes `filePath` of the arguments absolute, if it is not, from the first folder of the workspace (and `.` and `..`
/// in it are resolved, without looking at the file system). Left as it is if there is no folder.
fn absolute_file_path(arguments: &mut Value, folders: &[String]) {
    let Some(path) = arguments.get("filePath").and_then(Value::as_str) else {
        return;
    };
    if path.is_empty() || paths::is_absolute(path, paths::Flavor::NATIVE) {
        return;
    }
    let Some(first) = folders.first() else {
        return;
    };
    let joined = paths::join_normalized(first, path, paths::Flavor::NATIVE);
    arguments["filePath"] = Value::String(joined);
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
        .enumerate()
        .map(|(index, path)| {
            json!({
                "name": path.trim_end_matches('/').rsplit('/').next().unwrap_or(path),
                "uri": file_uri(path),
                "path": path,
                "index": index,
            })
        })
        .collect();
    json!({
        "success": true,
        "folders": entries,
        "rootPath": folders.first(),
        // The file of a multi-root workspace: there is none to name.
        "workspaceFile": Value::Null,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_come_with_their_index_and_there_is_no_workspace_file() {
        let text = workspace_folders_text(&["/a".to_owned(), "/b".to_owned()]);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["folders"][0]["index"], 0);
        assert_eq!(value["folders"][1]["index"], 1);
        assert_eq!(value["workspaceFile"], Value::Null);
        assert_eq!(value["rootPath"], "/a");
    }

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
    fn arguments_are_checked_against_the_schema_of_the_tool() {
        let schema = open_file_schema();
        assert_eq!(check_arguments(&schema, &json!({"filePath": "/a"})), Ok(()));
        // Optional ones may be left out, or null; extra ones are allowed.
        assert_eq!(
            check_arguments(
                &schema,
                &json!({"filePath": "/a", "preview": null, "other": 1})
            ),
            Ok(())
        );
        assert_eq!(
            check_arguments(&schema, &json!({})),
            Err("filePath is required".to_owned())
        );
        assert_eq!(
            check_arguments(&schema, &json!({"filePath": null})),
            Err("filePath is required".to_owned())
        );
        assert_eq!(
            check_arguments(&schema, &json!({"filePath": 3})),
            Err("filePath must be a string".to_owned())
        );
        assert_eq!(
            check_arguments(&schema, &json!({"filePath": "/a", "preview": "yes"})),
            Err("preview must be a boolean".to_owned())
        );
        assert!(check_arguments(&schema, &json!([1])).is_err());
        assert_eq!(check_arguments(&no_arguments(), &json!({})), Ok(()));
    }

    #[test]
    fn a_relative_path_is_taken_from_the_first_folder() {
        let folders = vec!["/home/u/app/".to_owned(), "/srv/www".to_owned()];
        let mut arguments = json!({"filePath": "src/../lib/./a.lua", "preview": true});
        absolute_file_path(&mut arguments, &folders);
        assert_eq!(arguments["filePath"], "/home/u/app/lib/a.lua");
        assert_eq!(arguments["preview"], true);
        // An absolute path, an empty one, and a call without folders are left as they are.
        for (given, folders) in [
            ("/etc/hosts", folders.as_slice()),
            ("", folders.as_slice()),
            ("a.lua", &[][..]),
        ] {
            let mut arguments = json!({ "filePath": given });
            absolute_file_path(&mut arguments, folders);
            assert_eq!(arguments["filePath"], given);
        }
    }

    #[test]
    fn every_tool_has_an_object_schema() {
        for spec in TOOLS {
            assert_eq!((spec.schema)()["type"], "object", "{}", spec.name);
        }
    }
}
