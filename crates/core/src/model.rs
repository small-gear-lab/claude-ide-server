//! The crate's own types for what an editor reports: selections, open editors, diagnostics.
//!
//! The adapter side speaks JSON, and Claude Code expects JSON of its own; this is the one place where the two are
//! converted, through these types. Above it (the tools, the workspace) nothing handles those messages as raw JSON.
//! What an adapter sends is checked: a missing field or a wrong type is a [`ModelError`], which the layers above turn
//! into a failed tool call or a dropped notification, never into a panic and never into a silently empty result.
//!
//! Positions follow the LSP convention: `line` is 0-based, `character` counts UTF-16 code units, and the end of a range
//! is exclusive.

use std::fmt;

use serde_json::{Map, Value, json};

/// What is wrong with a message from an adapter: where, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError {
    /// The place in the message, like `$.tabs[2].uri`.
    pub path: String,
    /// What is wrong there.
    pub reason: String,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for ModelError {}

fn error(path: &str, reason: &str) -> ModelError {
    ModelError {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

fn field<'a>(value: &'a Value, key: &str, path: &str) -> Result<&'a Value, ModelError> {
    value
        .as_object()
        .ok_or_else(|| error(path, "expected an object"))?
        .get(key)
        .ok_or_else(|| error(&format!("{path}.{key}"), "is missing"))
}

fn string(value: &Value, key: &str, path: &str) -> Result<String, ModelError> {
    field(value, key, path)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| error(&format!("{path}.{key}"), "expected a string"))
}

fn optional_string(value: &Value, key: &str, path: &str) -> Result<Option<String>, ModelError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(error(&format!("{path}.{key}"), "expected a string")),
    }
}

/// A path that starts at the root: Claude Code works out what to show from it, and a relative one would be taken
/// relative to the directory that Claude Code runs in, which is not that of the editor.
fn absolute_path(value: &Value, key: &str, path: &str) -> Result<String, ModelError> {
    let text = string(value, key, path)?;
    if text.starts_with('/') {
        Ok(text)
    } else {
        Err(error(&format!("{path}.{key}"), "expected an absolute path"))
    }
}

fn boolean(value: &Value, key: &str, path: &str) -> Result<bool, ModelError> {
    field(value, key, path)?
        .as_bool()
        .ok_or_else(|| error(&format!("{path}.{key}"), "expected a boolean"))
}

fn count(value: &Value, key: &str, path: &str) -> Result<u32, ModelError> {
    field(value, key, path)?
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| error(&format!("{path}.{key}"), "expected a non-negative integer"))
}

fn array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>, ModelError> {
    value
        .as_array()
        .ok_or_else(|| error(path, "expected an array"))
}

/// A place in a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0-based.
    pub line: u32,
    /// In UTF-16 code units.
    pub character: u32,
}

impl Position {
    fn from_adapter(value: &Value, path: &str) -> Result<Self, ModelError> {
        Ok(Self {
            line: count(value, "line", path)?,
            character: count(value, "character", path)?,
        })
    }

    fn to_json(self) -> Value {
        json!({ "line": self.line, "character": self.character })
    }
}

/// A stretch of a text. The end is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    fn from_adapter(value: &Value, path: &str) -> Result<Self, ModelError> {
        Ok(Self {
            start: Position::from_adapter(field(value, "start", path)?, &format!("{path}.start"))?,
            end: Position::from_adapter(field(value, "end", path)?, &format!("{path}.end"))?,
        })
    }

    /// Whether it holds nothing (the cursor, without a selection).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// As a selection is written: with `isEmpty`.
    fn to_selection_json(self) -> Value {
        json!({
            "start": self.start.to_json(),
            "end": self.end.to_json(),
            "isEmpty": self.is_empty(),
        })
    }

    /// As a diagnostic's range is written: the two ends.
    fn to_json(self) -> Value {
        json!({ "start": self.start.to_json(), "end": self.end.to_json() })
    }
}

/// What is selected in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The absolute path of the file.
    pub file_path: String,
    /// The selected text (empty if nothing is selected).
    pub text: String,
    pub range: Range,
}

impl Selection {
    /// From what an adapter sends in `selectionChanged`, and answers to `getCurrentSelection` and
    /// `getLatestSelection` when there is a selection: `{ filePath, text, selection: { start, end } }`.
    pub fn from_adapter(value: &Value) -> Result<Self, ModelError> {
        Ok(Self {
            file_path: absolute_path(value, "filePath", "$")?,
            text: string(value, "text", "$")?,
            range: Range::from_adapter(field(value, "selection", "$")?, "$.selection")?,
        })
    }

    /// The parameters of the `selection_changed` notification.
    pub fn to_notification(&self) -> Value {
        json!({
            "text": self.text,
            "filePath": self.file_path,
            "fileUrl": file_uri(&self.file_path),
            "selection": self.range.to_selection_json(),
        })
    }

    /// The text of the result of `getCurrentSelection` and `getLatestSelection`.
    pub fn to_tool_text(&self) -> String {
        json!({
            "success": true,
            "text": self.text,
            "filePath": self.file_path,
            "selection": self.range.to_selection_json(),
        })
        .to_string()
    }
}

/// What an adapter answers to `getCurrentSelection` or `getLatestSelection`: a selection, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionAnswer {
    Selection(Selection),
    /// There is none; the text says why (`{ "success": false, "message": ... }`).
    None(String),
}

impl SelectionAnswer {
    pub fn from_adapter(value: &Value) -> Result<Self, ModelError> {
        if value.get("success") == Some(&Value::Bool(false)) {
            let message = optional_string(value, "message", "$")?
                .unwrap_or_else(|| "No selection available".to_owned());
            return Ok(Self::None(message));
        }
        Selection::from_adapter(value).map(Self::Selection)
    }

    /// The text of the tool result.
    pub fn to_tool_text(&self) -> String {
        match self {
            Self::Selection(selection) => selection.to_tool_text(),
            Self::None(message) => json!({ "success": false, "message": message }).to_string(),
        }
    }
}

/// A file that is open in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenEditor {
    pub uri: String,
    pub is_active: bool,
    pub label: String,
    pub language_id: String,
    pub is_dirty: bool,
}

impl OpenEditor {
    fn from_adapter(value: &Value, path: &str) -> Result<Self, ModelError> {
        Ok(Self {
            uri: string(value, "uri", path)?,
            is_active: boolean(value, "isActive", path)?,
            label: string(value, "label", path)?,
            language_id: string(value, "languageId", path)?,
            is_dirty: boolean(value, "isDirty", path)?,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "uri": self.uri,
            "isActive": self.is_active,
            "label": self.label,
            "languageId": self.language_id,
            "isDirty": self.is_dirty,
        })
    }
}

/// From the answer to `getOpenEditors`: `{ tabs: [ ... ] }`.
pub fn open_editors_from_adapter(value: &Value) -> Result<Vec<OpenEditor>, ModelError> {
    array(field(value, "tabs", "$")?, "$.tabs")?
        .iter()
        .enumerate()
        .map(|(index, tab)| OpenEditor::from_adapter(tab, &format!("$.tabs[{index}]")))
        .collect()
}

/// The text of the result of `getOpenEditors`.
pub fn open_editors_to_tool_text(editors: &[OpenEditor]) -> String {
    json!({ "tabs": editors.iter().map(OpenEditor::to_json).collect::<Vec<_>>() }).to_string()
}

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    fn from_adapter(value: &Value, path: &str) -> Result<Self, ModelError> {
        match value.as_str() {
            Some("Error") => Ok(Self::Error),
            Some("Warning") => Ok(Self::Warning),
            Some("Information") => Ok(Self::Information),
            Some("Hint") => Ok(Self::Hint),
            _ => Err(error(
                path,
                "expected one of \"Error\", \"Warning\", \"Information\", \"Hint\"",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warning => "Warning",
            Self::Information => "Information",
            Self::Hint => "Hint",
        }
    }
}

/// One error or warning in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub severity: Severity,
    pub range: Range,
    /// What produced it (a language server, a linter), if the editor knows.
    pub source: Option<String>,
}

impl Diagnostic {
    fn from_adapter(value: &Value, path: &str) -> Result<Self, ModelError> {
        Ok(Self {
            message: string(value, "message", path)?,
            severity: Severity::from_adapter(
                field(value, "severity", path)?,
                &format!("{path}.severity"),
            )?,
            range: Range::from_adapter(field(value, "range", path)?, &format!("{path}.range"))?,
            source: optional_string(value, "source", path)?,
        })
    }

    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("message".to_owned(), json!(self.message));
        object.insert("severity".to_owned(), json!(self.severity.as_str()));
        object.insert("range".to_owned(), self.range.to_json());
        if let Some(source) = &self.source {
            object.insert("source".to_owned(), json!(source));
        }
        Value::Object(object)
    }
}

/// The diagnostics of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiagnostics {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// From the answer to `getDiagnostics`: `[ { uri, diagnostics: [ ... ] } ]`.
pub fn diagnostics_from_adapter(value: &Value) -> Result<Vec<FileDiagnostics>, ModelError> {
    array(value, "$")?
        .iter()
        .enumerate()
        .map(|(file_index, file)| {
            let path = format!("$[{file_index}]");
            let diagnostics = array(
                field(file, "diagnostics", &path)?,
                &format!("{path}.diagnostics"),
            )?
            .iter()
            .enumerate()
            .map(|(index, diagnostic)| {
                Diagnostic::from_adapter(diagnostic, &format!("{path}.diagnostics[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
            Ok(FileDiagnostics {
                uri: string(file, "uri", &path)?,
                diagnostics,
            })
        })
        .collect()
}

/// The text of the result of `getDiagnostics`.
pub fn diagnostics_to_tool_text(files: &[FileDiagnostics]) -> String {
    Value::Array(
        files
            .iter()
            .map(|file| {
                json!({
                    "uri": file.uri,
                    "diagnostics": file.diagnostics.iter().map(Diagnostic::to_json).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
    .to_string()
}

/// The user sent a selection, or a file, to Claude as a reference (`atMention`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtMention {
    pub file_path: String,
    /// 0-based.
    pub line_start: u32,
    /// 0-based; the last line that is mentioned.
    pub line_end: u32,
}

impl AtMention {
    pub fn from_adapter(value: &Value) -> Result<Self, ModelError> {
        Ok(Self {
            file_path: absolute_path(value, "filePath", "$")?,
            line_start: count(value, "lineStart", "$")?,
            line_end: count(value, "lineEnd", "$")?,
        })
    }

    /// The parameters of the `at_mentioned` notification.
    pub fn to_notification(&self) -> Value {
        json!({
            "filePath": self.file_path,
            "lineStart": self.line_start,
            "lineEnd": self.line_end,
        })
    }
}

/// `file://` and the path, with every byte that is not safe in a URI written as `%XX` (the `/` stays).
pub(crate) fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(byte));
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// What came of looking for the text that `openFile` was given (`startText`, `endText`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMatch {
    /// The text of `startText` is selected (no `endText` was given).
    Text,
    /// The text from `startText` to `endText` is selected.
    Range,
    /// `startText` was found and the cursor is there, but `endText` was not found.
    Positioned,
    /// `startText` was not found.
    NotFound,
}

impl TextMatch {
    fn from_adapter(value: &Value, path: &str) -> Result<Self, ModelError> {
        match value.as_str() {
            Some("text") => Ok(Self::Text),
            Some("range") => Ok(Self::Range),
            Some("positioned") => Ok(Self::Positioned),
            Some("not_found") => Ok(Self::NotFound),
            _ => Err(error(
                path,
                "expected one of \"text\", \"range\", \"positioned\", \"not_found\"",
            )),
        }
    }
}

/// What `openFile` was asked: the parts of its arguments that the text of the result depends on.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenFileRequest<'a> {
    pub start_text: Option<&'a str>,
    pub end_text: Option<&'a str>,
    /// Whether the file is made the active one (`makeFrontmost`, true unless it says false).
    pub make_frontmost: bool,
}

impl<'a> OpenFileRequest<'a> {
    /// From the arguments of the tool call. An empty text is none, as in the VS Code extension.
    pub fn from_arguments(arguments: &'a Value) -> Self {
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        };
        Self {
            start_text: text("startText"),
            end_text: text("endText"),
            make_frontmost: arguments.get("makeFrontmost") != Some(&Value::Bool(false)),
        }
    }
}

/// A file that the editor opened for `openFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedFile {
    /// The absolute path of the file.
    pub file_path: String,
    pub language_id: String,
    pub line_count: u32,
    pub is_dirty: bool,
    pub is_untitled: bool,
    /// What came of `startText`, if one was given.
    pub text_match: Option<TextMatch>,
}

impl OpenedFile {
    /// From the answer of an adapter to `openFile`:
    /// `{ filePath, languageId, lineCount, isDirty, isUntitled, selection? }`, where `selection` says what came of `startText`
    /// (`"text"`, `"range"`, `"positioned"` or `"not_found"`) and must be there if `startText` was given.
    pub fn from_adapter(value: &Value, asked_for_text: bool) -> Result<Self, ModelError> {
        let text_match = if asked_for_text {
            Some(TextMatch::from_adapter(
                field(value, "selection", "$")?,
                "$.selection",
            )?)
        } else {
            None
        };
        Ok(Self {
            file_path: absolute_path(value, "filePath", "$")?,
            language_id: string(value, "languageId", "$")?,
            line_count: count(value, "lineCount", "$")?,
            is_dirty: boolean(value, "isDirty", "$")?,
            is_untitled: boolean(value, "isUntitled", "$")?,
            text_match,
        })
    }

    /// The text of the result of `openFile`, as the VS Code extension writes it: the message alone when `startText` was
    /// given or the file is made the frontmost one, and the details as JSON when it is not.
    pub fn to_tool_text(&self, request: &OpenFileRequest) -> String {
        let start = request.start_text.unwrap_or_default();
        let end = request.end_text.unwrap_or_default();
        if let Some(text_match) = self.text_match {
            return match text_match {
                TextMatch::Text => format!("Opened file and selected text \"{start}\""),
                TextMatch::Range => {
                    format!("Opened file and selected text from \"{start}\" to \"{end}\"")
                }
                TextMatch::Positioned => format!(
                    "Opened file and positioned at \"{start}\" (end text \"{end}\" not found)"
                ),
                TextMatch::NotFound => format!("Opened file, but text \"{start}\" not found"),
            };
        }
        let message = format!("Opened file: {}", self.file_path);
        if request.make_frontmost {
            return message;
        }
        json!({
            "success": true,
            "filePath": self.file_path,
            "fileUrl": file_uri(&self.file_path),
            "message": message,
            "languageId": self.language_id,
            "lineCount": self.line_count,
            "isDirty": self.is_dirty,
            "isUntitled": self.is_untitled,
            "isClosed": false,
        })
        .to_string()
    }
}

/// Whether the answer of an adapter says that the document is not open: `{ "success": false }`, with a message that
/// the server does not need (it says which file, since it knows).
fn says_not_open(value: &Value) -> bool {
    value.get("success") == Some(&Value::Bool(false))
}

/// The text of a result that says that a document is not open, as the VS Code extension writes it.
fn not_open_text(file_path: &str) -> String {
    json!({ "success": false, "message": format!("Document not open: {file_path}") }).to_string()
}

/// What an adapter answers to `checkDocumentDirty`: `{ isDirty, isUntitled }`, or `{ success: false }` if the document is
/// not open. Returns `None` for a document that is not open.
pub fn document_state_from_adapter(value: &Value) -> Result<Option<(bool, bool)>, ModelError> {
    if says_not_open(value) {
        return Ok(None);
    }
    Ok(Some((
        boolean(value, "isDirty", "$")?,
        boolean(value, "isUntitled", "$")?,
    )))
}

/// The text of the result of `checkDocumentDirty` for the file `file_path` (as the caller wrote it).
pub fn document_state_to_tool_text(file_path: &str, state: Option<(bool, bool)>) -> String {
    match state {
        Some((is_dirty, is_untitled)) => json!({
            "success": true,
            "filePath": file_path,
            "isDirty": is_dirty,
            "isUntitled": is_untitled,
        })
        .to_string(),
        None => not_open_text(file_path),
    }
}

/// What an adapter answers to `saveDocument`: `{ saved }`, or `{ success: false }` if the document is not open.
/// Returns `None` for a document that is not open.
pub fn save_from_adapter(value: &Value) -> Result<Option<bool>, ModelError> {
    if says_not_open(value) {
        return Ok(None);
    }
    Ok(Some(boolean(value, "saved", "$")?))
}

/// The text of the result of `saveDocument` for the file `file_path` (as the caller wrote it).
pub fn save_to_tool_text(file_path: &str, saved: Option<bool>) -> String {
    match saved {
        Some(saved) => json!({
            "success": true,
            "filePath": file_path,
            "saved": saved,
            "message": if saved { "Document saved successfully" } else { "Document was not dirty or save failed" },
        })
        .to_string(),
        None => not_open_text(file_path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection_json() -> Value {
        json!({
            "filePath": "/home/u/a b.lua",
            "text": "日本語",
            "selection": {
                "start": {"line": 3, "character": 0},
                "end": {"line": 3, "character": 3},
            },
        })
    }

    #[test]
    fn a_selection_becomes_a_notification_and_a_tool_result() {
        let selection = Selection::from_adapter(&selection_json()).unwrap();
        assert_eq!(selection.range.end.character, 3);
        assert!(!selection.range.is_empty());

        let notification = selection.to_notification();
        assert_eq!(notification["fileUrl"], "file:///home/u/a%20b.lua");
        assert_eq!(notification["selection"]["isEmpty"], false);
        assert_eq!(notification["text"], "日本語");

        let result: Value = serde_json::from_str(&selection.to_tool_text()).unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["selection"]["start"]["line"], 3);
    }

    #[test]
    fn a_cursor_without_a_selection_is_an_empty_range() {
        let mut value = selection_json();
        value["text"] = json!("");
        value["selection"]["end"] = value["selection"]["start"].clone();
        let selection = Selection::from_adapter(&value).unwrap();
        assert!(selection.range.is_empty());
        assert_eq!(selection.to_notification()["selection"]["isEmpty"], true);
    }

    #[test]
    fn an_answer_that_says_there_is_no_selection_is_not_an_error() {
        let none = SelectionAnswer::from_adapter(
            &json!({"success": false, "message": "No active editor found"}),
        )
        .unwrap();
        assert_eq!(
            none,
            SelectionAnswer::None("No active editor found".to_owned())
        );
        let text: Value = serde_json::from_str(&none.to_tool_text()).unwrap();
        assert_eq!(
            text,
            json!({"success": false, "message": "No active editor found"})
        );

        let default = SelectionAnswer::from_adapter(&json!({"success": false})).unwrap();
        assert_eq!(
            default,
            SelectionAnswer::None("No selection available".to_owned())
        );
    }

    #[test]
    fn malformed_selections_say_where_they_are_wrong() {
        let cases = [
            (json!({}), "$.filePath"),
            (
                json!({"filePath": 1, "text": "", "selection": {}}),
                "$.filePath",
            ),
            (
                json!({"filePath": "/a", "text": "", "selection": {}}),
                "$.selection.start",
            ),
            (
                json!({"filePath": "/a", "text": "", "selection": {"start": {"line": -1, "character": 0}, "end": {"line": 0, "character": 0}}}),
                "$.selection.start.line",
            ),
            (
                json!({"filePath": "/a", "text": "", "selection": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": "x"}}}),
                "$.selection.end.character",
            ),
            (json!([1]), "$"),
        ];
        for (value, path) in cases {
            let error = SelectionAnswer::from_adapter(&value).unwrap_err();
            assert_eq!(error.path, path, "{value}: {error}");
        }
    }

    #[test]
    fn open_editors_round_trip_and_the_errors_name_the_tab() {
        let value = json!({"tabs": [
            {"uri": "file:///a.lua", "isActive": true, "label": "a.lua", "languageId": "lua", "isDirty": false},
            {"uri": "file:///b.rs", "isActive": false, "label": "b.rs", "languageId": "rust", "isDirty": true},
        ]});
        let editors = open_editors_from_adapter(&value).unwrap();
        assert_eq!(editors.len(), 2);
        assert!(editors[1].is_dirty);
        let text: Value = serde_json::from_str(&open_editors_to_tool_text(&editors)).unwrap();
        assert_eq!(text, value);

        assert_eq!(open_editors_from_adapter(&json!({"tabs": []})).unwrap(), []);
        let mut broken = value.clone();
        broken["tabs"][1]["isDirty"] = json!("yes");
        assert_eq!(
            open_editors_from_adapter(&broken).unwrap_err().path,
            "$.tabs[1].isDirty"
        );
        assert_eq!(
            open_editors_from_adapter(&json!({})).unwrap_err().path,
            "$.tabs"
        );
        assert_eq!(
            open_editors_from_adapter(&json!({"tabs": {}}))
                .unwrap_err()
                .path,
            "$.tabs"
        );
    }

    #[test]
    fn diagnostics_round_trip_and_the_errors_name_the_diagnostic() {
        let value = json!([{
            "uri": "file:///a.lua",
            "diagnostics": [
                {"message": "unused", "severity": "Warning", "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}}, "source": "lua_ls"},
                {"message": "bad", "severity": "Error", "range": {"start": {"line": 4, "character": 0}, "end": {"line": 4, "character": 1}}},
            ],
        }]);
        let files = diagnostics_from_adapter(&value).unwrap();
        assert_eq!(files[0].diagnostics[0].severity, Severity::Warning);
        assert_eq!(files[0].diagnostics[1].source, None);
        let text: Value = serde_json::from_str(&diagnostics_to_tool_text(&files)).unwrap();
        assert_eq!(text[0]["diagnostics"][0]["source"], "lua_ls");
        assert!(text[0]["diagnostics"][1].get("source").is_none());
        assert_eq!(text[0]["diagnostics"][1]["severity"], "Error");

        assert_eq!(diagnostics_from_adapter(&json!([])).unwrap(), []);
        let mut broken = value.clone();
        broken[0]["diagnostics"][1]["severity"] = json!("Fatal");
        assert_eq!(
            diagnostics_from_adapter(&broken).unwrap_err().path,
            "$[0].diagnostics[1].severity"
        );
        assert_eq!(diagnostics_from_adapter(&json!({})).unwrap_err().path, "$");
        assert_eq!(
            diagnostics_from_adapter(&json!([{"uri": "x"}]))
                .unwrap_err()
                .path,
            "$[0].diagnostics"
        );
    }

    #[test]
    fn a_file_path_that_is_not_absolute_is_refused() {
        let mut value = selection_json();
        value["filePath"] = json!("src/a.lua");
        let error = Selection::from_adapter(&value).unwrap_err();
        assert_eq!(error.path, "$.filePath");
        assert!(error.reason.contains("absolute"), "{error}");

        let error =
            AtMention::from_adapter(&json!({"filePath": "a.lua", "lineStart": 0, "lineEnd": 0}))
                .unwrap_err();
        assert_eq!(error.path, "$.filePath");
    }

    #[test]
    fn an_at_mention_becomes_a_notification() {
        let mention =
            AtMention::from_adapter(&json!({"filePath": "/a.lua", "lineStart": 4, "lineEnd": 9}))
                .unwrap();
        assert_eq!(
            mention.to_notification(),
            json!({"filePath": "/a.lua", "lineStart": 4, "lineEnd": 9})
        );
        assert_eq!(
            AtMention::from_adapter(&json!({"filePath": "/a.lua", "lineStart": 4}))
                .unwrap_err()
                .path,
            "$.lineEnd"
        );
    }

    #[test]
    fn a_file_uri_encodes_what_is_not_safe_and_keeps_the_slashes() {
        assert_eq!(
            file_uri("/home/u/my project"),
            "file:///home/u/my%20project"
        );
        assert_eq!(file_uri("/a/日本"), "file:///a/%E6%97%A5%E6%9C%AC");
    }

    #[test]
    fn an_opened_file_is_text_or_details_as_the_extension_writes_it() {
        let answer = json!({"filePath": "/a/b.lua", "languageId": "lua", "lineCount": 12, "isDirty": false, "isUntitled": false});
        let opened = OpenedFile::from_adapter(&answer, false).unwrap();
        let arguments = json!({"filePath": "/a/b.lua"});
        let frontmost = OpenFileRequest::from_arguments(&arguments);
        assert_eq!(opened.to_tool_text(&frontmost), "Opened file: /a/b.lua");
        let arguments = json!({"makeFrontmost": false});
        let behind = OpenFileRequest::from_arguments(&arguments);
        let details: Value = serde_json::from_str(&opened.to_tool_text(&behind)).unwrap();
        assert_eq!(
            details,
            json!({"success": true, "filePath": "/a/b.lua", "fileUrl": "file:///a/b.lua", "message": "Opened file: /a/b.lua",
                   "languageId": "lua", "lineCount": 12, "isDirty": false, "isUntitled": false, "isClosed": false})
        );
        assert_eq!(
            OpenedFile::from_adapter(&json!({"filePath": "b.lua", "languageId": "lua", "lineCount": 1, "isDirty": false, "isUntitled": false}), false)
                .unwrap_err()
                .path,
            "$.filePath"
        );
        assert_eq!(
            OpenedFile::from_adapter(&json!({"filePath": "/b.lua", "languageId": "lua", "lineCount": 1, "isUntitled": false}), false)
                .unwrap_err()
                .path,
            "$.isDirty"
        );
    }

    #[test]
    fn what_came_of_the_text_to_select_is_a_message() {
        let with = |selection: &str| json!({"filePath": "/a.lua", "languageId": "lua", "lineCount": 3, "isDirty": false, "isUntitled": false, "selection": selection});
        let arguments = json!({"startText": "foo", "endText": "bar"});
        let request = OpenFileRequest::from_arguments(&arguments);
        let text = |selection: &str| {
            OpenedFile::from_adapter(&with(selection), true)
                .unwrap()
                .to_tool_text(&request)
        };
        assert_eq!(
            text("range"),
            "Opened file and selected text from \"foo\" to \"bar\""
        );
        assert_eq!(
            text("positioned"),
            "Opened file and positioned at \"foo\" (end text \"bar\" not found)"
        );
        assert_eq!(text("not_found"), "Opened file, but text \"foo\" not found");
        assert_eq!(text("text"), "Opened file and selected text \"foo\"");
        // The adapter must say what came of it when it was asked, and with one of the four words.
        let plain = json!({"filePath": "/a.lua", "languageId": "lua", "lineCount": 3, "isDirty": false, "isUntitled": false});
        assert_eq!(
            OpenedFile::from_adapter(&plain, true).unwrap_err().path,
            "$.selection"
        );
        assert_eq!(
            OpenedFile::from_adapter(&with("maybe"), true)
                .unwrap_err()
                .path,
            "$.selection"
        );
        // An empty text is none.
        let arguments = json!({"startText": ""});
        let empty = OpenFileRequest::from_arguments(&arguments);
        assert!(empty.start_text.is_none());
    }

    #[test]
    fn the_state_of_a_document_and_a_document_that_is_not_open() {
        assert_eq!(
            document_state_from_adapter(&json!({"isDirty": true, "isUntitled": false})),
            Ok(Some((true, false)))
        );
        assert_eq!(
            document_state_from_adapter(&json!({"success": false})),
            Ok(None)
        );
        assert_eq!(
            document_state_from_adapter(&json!({"isDirty": true}))
                .unwrap_err()
                .path,
            "$.isUntitled"
        );
        let open: Value =
            serde_json::from_str(&document_state_to_tool_text("/a", Some((true, false)))).unwrap();
        assert_eq!(
            open,
            json!({"success": true, "filePath": "/a", "isDirty": true, "isUntitled": false})
        );
        let closed: Value = serde_json::from_str(&document_state_to_tool_text("/a", None)).unwrap();
        assert_eq!(
            closed,
            json!({"success": false, "message": "Document not open: /a"})
        );
    }

    #[test]
    fn saving_a_document_and_a_document_that_is_not_open() {
        assert_eq!(save_from_adapter(&json!({"saved": true})), Ok(Some(true)));
        assert_eq!(save_from_adapter(&json!({"success": false})), Ok(None));
        assert_eq!(save_from_adapter(&json!({})).unwrap_err().path, "$.saved");
        let saved: Value = serde_json::from_str(&save_to_tool_text("/a", Some(true))).unwrap();
        assert_eq!(
            saved,
            json!({"success": true, "filePath": "/a", "saved": true, "message": "Document saved successfully"})
        );
        let not_saved: Value = serde_json::from_str(&save_to_tool_text("/a", Some(false))).unwrap();
        assert_eq!(not_saved["saved"], false);
        let closed: Value = serde_json::from_str(&save_to_tool_text("/a", None)).unwrap();
        assert_eq!(closed["message"], "Document not open: /a");
    }
}
