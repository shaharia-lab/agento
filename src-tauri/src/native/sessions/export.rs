//! One session, written out as a file the user chose (#591): Markdown, JSONL
//! or plain text, each filtered by the same five scope toggles.
//!
//! **This is a Tauri command's back half, not an `/api` route**, for the reason
//! `logs.rs` gives for its own export: the destination comes from the native
//! Save-As dialog and the bytes go straight to it, so round-tripping the whole
//! transcript through the HTTP proxy only to write it next to where it came
//! from buys nothing. `lib.rs::export_session` is the command.
//!
//! **The content comes from the transcript itself, not from
//! [`super::detail::get`].** The detail read is a rendering payload and it is
//! lossy on purpose: it drops `system` events and image blocks, and caps a
//! tool result at 2000 characters. An export is the one reader that wants all
//! of it, so this walks the JSONL directly. The detail read is still what
//! supplies the metadata header, because cost and the display title live in
//! the cache and it already knows how to patch them in.
//!
//! What each toggle governs, identically in all three formats:
//!
//! - **reasoning** — `thinking` / `redacted_thinking` blocks;
//! - **tool calls** — `tool_use` blocks: the tool's name and its input;
//! - **tool results** — `tool_result` blocks, *and* every binary attachment
//!   (`image` / `document`), wherever it sits;
//! - **system** — `system` events, and user events Claude Code marks as
//!   injected (`isMeta`, `isCompactSummary`) rather than typed;
//! - **metadata** — a header: title, id, project, model, times, tokens, cost.
//!
//! With everything off an export is the conversation's prose: what the user
//! typed and what the assistant wrote back. Sidechain events are skipped in
//! every format — they are delegated sub-agent work, which the parent
//! transcript does not render either.
//!
//! **Binary content is never inlined.** In Markdown and text an attachment is
//! decoded into `<stem>-attachments/` beside the exported file and linked by
//! relative path. JSONL is the raw event stream, so an attachment it keeps
//! stays exactly as Claude Code wrote it.
//!
//! **A JSONL line the filters did not touch is written byte for byte.** One
//! they did is re-assembled from [`Obj`], which keeps key order and every
//! value's spelling — a `serde_json::Value` would sort the keys (this crate
//! does not enable `preserve_order`) and respell the numbers.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use super::detail::{self, SessionDetail};
use crate::native::settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Markdown,
    Jsonl,
    Text,
}

/// The dialog's choices. Every toggle defaults to off, as the panel does.
#[derive(Debug, Clone, Deserialize)]
pub struct ExportOptions {
    pub format: Format,
    #[serde(default)]
    pub include_reasoning: bool,
    #[serde(default)]
    pub include_tool_calls: bool,
    #[serde(default)]
    pub include_tool_results: bool,
    #[serde(default)]
    pub include_system: bool,
    #[serde(default)]
    pub include_metadata: bool,
}

/// What the command reports back, so the panel can say what it wrote.
#[derive(Debug, Serialize)]
pub struct ExportResult {
    pub path: String,
    pub bytes: u64,
    pub attachments: u64,
    /// The sibling folder, when anything was written to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments_dir: Option<String>,
}

/// Export `session_id` to `dest`.
///
/// Attachments are written before the main file, so a file that exists is a
/// file whose links resolve.
pub fn export(
    db_path: &Path,
    session_id: &str,
    dest: &Path,
    opts: &ExportOptions,
) -> Result<ExportResult, String> {
    let file = locate(db_path, session_id)?.ok_or_else(|| "session not found".to_string())?;
    let meta = if opts.include_metadata {
        detail::get(db_path, session_id)?
    } else {
        None
    };
    let lines = read_lines(&file)?;

    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".to_string());
    let dir_name = format!("{stem}-attachments");
    let out = build(&lines, opts, meta.as_ref(), &dir_name);

    let mut attachments_dir = None;
    if !out.attachments.is_empty() {
        let dir = dest
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
            .join(&dir_name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        for a in &out.attachments {
            let path = dir.join(&a.name);
            std::fs::write(&path, &a.bytes)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
        attachments_dir = Some(dir.to_string_lossy().into_owned());
    }
    std::fs::write(dest, out.body.as_bytes())
        .map_err(|e| format!("writing {}: {e}", dest.display()))?;

    Ok(ExportResult {
        path: dest.to_string_lossy().into_owned(),
        bytes: out.body.len() as u64,
        attachments: out.attachments.len() as u64,
        attachments_dir,
    })
}

/// The transcript's path, found the way the detail read finds it.
fn locate(db_path: &Path, session_id: &str) -> Result<Option<PathBuf>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let dirs = settings::load(&conn).indexed_config_dirs;
    Ok(detail::find_session_file(&dirs, session_id).map(|(_, _, file)| file))
}

fn read_lines(path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("opening transcript {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| format!("reading transcript {}: {e}", path.display()))?;
        if !line.trim().is_empty() {
            out.push(line);
        }
    }
    Ok(out)
}

/// A decoded attachment, named relative to the attachments folder.
#[derive(Debug)]
pub struct Attachment {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct Built {
    pub body: String,
    pub attachments: Vec<Attachment>,
}

/// Everything short of the filesystem: the transcript's lines in, the file's
/// content and its attachments out. `dir_name` is the folder the links point
/// into, relative to the exported file.
pub fn build(
    lines: &[String],
    opts: &ExportOptions,
    meta: Option<&SessionDetail>,
    dir_name: &str,
) -> Built {
    match opts.format {
        Format::Jsonl => Built {
            body: build_jsonl(lines, opts, meta),
            attachments: Vec::new(),
        },
        Format::Markdown | Format::Text => {
            let mut doc = Doc::new(opts, dir_name);
            for line in lines {
                if let Ok(ev) = serde_json::from_str::<Event>(line) {
                    doc.event(&ev);
                }
            }
            let body = if opts.format == Format::Markdown {
                render_markdown(&doc.items, meta)
            } else {
                render_text(&doc.items, meta)
            };
            Built {
                body,
                attachments: doc.attachments,
            }
        }
    }
}

// --- The transcript, as far as an export reads it -------------------------

/// One transcript line. Every field is optional because a JSON `null` has to
/// read as absent rather than fail the line.
#[derive(Deserialize)]
struct Event {
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default, rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(default, rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(default, rename = "isCompactSummary")]
    is_compact_summary: Option<bool>,
    /// A `system` event's own text.
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(default, rename = "type")]
    block_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Box<RawValue>>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    content: Option<Box<RawValue>>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Deserialize)]
struct Source {
    #[serde(default, rename = "type")]
    source_type: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// A message's content is a string or an array of blocks.
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

fn content_of(raw: Option<&RawValue>) -> Content {
    let Some(raw) = raw else {
        return Content::Blocks(Vec::new());
    };
    if let Ok(s) = serde_json::from_str::<String>(raw.get()) {
        return Content::Text(s);
    }
    let blocks = serde_json::from_str::<Vec<Box<RawValue>>>(raw.get())
        .unwrap_or_default()
        .iter()
        .filter_map(|b| serde_json::from_str::<Block>(b.get()).ok())
        .collect();
    Content::Blocks(blocks)
}

/// Which toggle a content block answers to. `None` is prose, which always
/// stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Reasoning,
    ToolCall,
    ToolResult,
}

fn kind_of(block_type: &str) -> Option<Kind> {
    match block_type {
        "thinking" | "redacted_thinking" => Some(Kind::Reasoning),
        "tool_use" | "server_tool_use" | "mcp_tool_use" => Some(Kind::ToolCall),
        // Binary content shares the tool-results toggle (#591's
        // "tool results / binary attachments").
        "image" | "document" => Some(Kind::ToolResult),
        t if t == "tool_result" || t.ends_with("_tool_result") => Some(Kind::ToolResult),
        _ => None,
    }
}

impl ExportOptions {
    fn allows(&self, kind: Option<Kind>) -> bool {
        match kind {
            None => true,
            Some(Kind::Reasoning) => self.include_reasoning,
            Some(Kind::ToolCall) => self.include_tool_calls,
            Some(Kind::ToolResult) => self.include_tool_results,
        }
    }
}

/// An injected user event: text Claude Code wrote into the user's slot.
fn is_injected(ev: &Event) -> bool {
    ev.is_meta == Some(true) || ev.is_compact_summary == Some(true)
}

// --- Markdown and text: one model, two renderings -------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        }
    }
}

#[derive(Debug)]
enum Item {
    /// A new section, emitted only when the speaker changes — Claude Code
    /// writes one assistant event per block, and a heading per event would
    /// bury the conversation.
    Section(Role),
    Text(String),
    Thinking(String),
    ToolCall {
        name: String,
        input: String,
    },
    ToolResult {
        name: String,
        text: String,
        is_error: bool,
    },
    /// A link to a file under the attachments folder, or to a URL.
    Attachment {
        target: String,
        is_image: bool,
    },
}

struct Doc<'a> {
    opts: &'a ExportOptions,
    dir_name: &'a str,
    items: Vec<Item>,
    attachments: Vec<Attachment>,
    role: Option<Role>,
    /// `tool_use` id → tool name, so a result can say which call it answers.
    tools: std::collections::HashMap<String, String>,
}

impl<'a> Doc<'a> {
    fn new(opts: &'a ExportOptions, dir_name: &'a str) -> Self {
        Doc {
            opts,
            dir_name,
            items: Vec::new(),
            attachments: Vec::new(),
            role: None,
            tools: std::collections::HashMap::new(),
        }
    }

    fn speak(&mut self, role: Role) {
        if self.role != Some(role) {
            self.items.push(Item::Section(role));
            self.role = Some(role);
        }
    }

    /// A tool result or an attachment inside one belongs to whoever is
    /// already speaking — it answers the assistant's call, it is not a turn.
    fn continue_or(&mut self, role: Role) {
        if self.role.is_none() {
            self.speak(role);
        }
    }

    fn event(&mut self, ev: &Event) {
        if ev.is_sidechain == Some(true) {
            return;
        }
        match ev.event_type.as_deref() {
            Some("system") => {
                if !self.opts.include_system {
                    return;
                }
                let text = ev
                    .content
                    .clone()
                    .filter(|c| !c.trim().is_empty())
                    .or_else(|| ev.subtype.clone())
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    self.speak(Role::System);
                    self.items.push(Item::Text(text));
                }
            }
            Some("user") if is_injected(ev) => {
                if !self.opts.include_system {
                    return;
                }
                let content = content_of(ev.message.as_ref().and_then(|m| m.content.as_deref()));
                let text = prose(&content);
                if !text.trim().is_empty() {
                    self.speak(Role::System);
                    self.items.push(Item::Text(text));
                }
            }
            Some("user") => self.message(ev, Role::User),
            Some("assistant") => self.message(ev, Role::Assistant),
            _ => {}
        }
    }

    fn message(&mut self, ev: &Event, role: Role) {
        let content = content_of(ev.message.as_ref().and_then(|m| m.content.as_deref()));
        let blocks = match content {
            Content::Text(s) => {
                if !s.trim().is_empty() {
                    self.speak(role);
                    self.items.push(Item::Text(s));
                }
                return;
            }
            Content::Blocks(b) => b,
        };
        for b in blocks {
            let t = b.block_type.as_deref().unwrap_or("");
            let kind = kind_of(t);
            // Recorded whether or not calls are exported, so results can
            // still be named when only they are.
            if kind == Some(Kind::ToolCall) {
                if let (Some(id), Some(name)) = (&b.id, &b.name) {
                    self.tools.insert(id.clone(), name.clone());
                }
            }
            if !self.opts.allows(kind) {
                continue;
            }
            match t {
                "text" => {
                    let text = b.text.unwrap_or_default();
                    if !text.trim().is_empty() {
                        self.speak(role);
                        self.items.push(Item::Text(text));
                    }
                }
                "thinking" => {
                    // Models may redact the text and keep only the signature;
                    // an empty block has nothing to export.
                    let text = b.thinking.unwrap_or_default();
                    if !text.trim().is_empty() {
                        self.speak(role);
                        self.items.push(Item::Thinking(text));
                    }
                }
                "redacted_thinking" => {}
                "image" | "document" => {
                    self.speak(role);
                    self.attachment(b.source.as_ref(), t == "image");
                }
                _ if kind == Some(Kind::ToolCall) => {
                    self.speak(role);
                    self.items.push(Item::ToolCall {
                        name: b.name.unwrap_or_default(),
                        input: b
                            .input
                            .as_deref()
                            .map(|r| pretty_json(r.get()))
                            .unwrap_or_default(),
                    });
                }
                _ if kind == Some(Kind::ToolResult) => {
                    self.continue_or(role);
                    let name = b
                        .tool_use_id
                        .as_ref()
                        .and_then(|id| self.tools.get(id).cloned())
                        .unwrap_or_default();
                    let mut text = String::new();
                    let mut binary = Vec::new();
                    match content_of(b.content.as_deref()) {
                        Content::Text(s) => text = s,
                        Content::Blocks(inner) => {
                            for ib in inner {
                                match ib.block_type.as_deref() {
                                    Some("text") => {
                                        if !text.is_empty() {
                                            text.push('\n');
                                        }
                                        text.push_str(ib.text.as_deref().unwrap_or(""));
                                    }
                                    Some(t @ ("image" | "document")) => {
                                        binary.push((ib.source, t == "image"))
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    self.items.push(Item::ToolResult {
                        name,
                        text,
                        is_error: b.is_error == Some(true),
                    });
                    for (source, is_image) in binary {
                        self.attachment(source.as_ref(), is_image);
                    }
                }
                // Any other block type is not something a reader can use.
                _ => {}
            }
        }
    }

    /// Decode an attachment into the folder, or link its URL. Never inlined.
    fn attachment(&mut self, source: Option<&Source>, is_image: bool) {
        let Some(source) = source else { return };
        match source.source_type.as_deref() {
            Some("url") => {
                if let Some(url) = &source.url {
                    self.items.push(Item::Attachment {
                        target: url.clone(),
                        is_image,
                    });
                }
            }
            Some("base64") | Some("text") => {
                let data = source.data.as_deref().unwrap_or("");
                let bytes = if source.source_type.as_deref() == Some("text") {
                    Some(data.as_bytes().to_vec())
                } else {
                    base64::engine::general_purpose::STANDARD.decode(data).ok()
                };
                let Some(bytes) = bytes else {
                    self.items.push(Item::Text(
                        "[an attachment could not be decoded]".to_string(),
                    ));
                    return;
                };
                let ext = extension(source.media_type.as_deref().unwrap_or(""));
                let name = format!("attachment-{:03}.{ext}", self.attachments.len() + 1);
                self.items.push(Item::Attachment {
                    target: format!("{}/{name}", self.dir_name),
                    is_image,
                });
                self.attachments.push(Attachment { name, bytes });
            }
            _ => {}
        }
    }
}

/// The prose of a message: its string, or its text blocks joined.
fn prose(content: &Content) -> String {
    match content {
        Content::Text(s) => s.clone(),
        Content::Blocks(blocks) => blocks
            .iter()
            .filter(|b| b.block_type.as_deref() == Some("text"))
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "application/json" => "json",
        _ => "bin",
    }
}

/// Indent compact JSON without re-encoding it: key order, number spelling and
/// string escapes all survive, which a `serde_json::Value` round trip would
/// not guarantee.
fn pretty_json(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = raw.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                // An empty container stays on one line.
                while chars.peek().is_some_and(|n| n.is_whitespace()) {
                    chars.next();
                }
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    out.push(chars.next().unwrap_or(' '));
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// A fence long enough that nothing inside it can close it.
fn fence(body: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in body.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

fn fenced(out: &mut String, lang: &str, body: &str) {
    let f = fence(body);
    let _ = writeln!(out, "{f}{lang}\n{}\n{f}\n", body.trim_end_matches('\n'));
}

struct MetaLine {
    label: &'static str,
    value: String,
}

fn meta_lines(d: &SessionDetail) -> (String, Vec<MetaLine>) {
    let s = &d.summary;
    let title = if s.display_title.is_empty() {
        s.session_id.clone()
    } else {
        s.display_title.clone()
    };
    let mut lines = vec![MetaLine {
        label: "Session",
        value: s.session_id.clone(),
    }];
    let mut push = |label, value: String| {
        if !value.is_empty() {
            lines.push(MetaLine { label, value });
        }
    };
    push("Project", s.project_path.clone());
    push("Branch", s.git_branch.clone());
    push("Model", s.model.clone());
    let start = s.start_time.0;
    let last = s.last_activity.0;
    let known = start.timestamp() > 0;
    if known {
        push("Started", start.to_rfc3339());
        push("Last activity", last.to_rfc3339());
        push("Duration", duration((last - start).num_seconds()));
    }
    push("Messages", s.message_count.to_string());
    let u = &s.usage;
    push(
        "Tokens",
        format!(
            "{} input · {} output · {} cache read · {} cache write",
            u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_creation_tokens
        ),
    );
    let cost = s.cost.total_usd + s.subagent_cost.total_usd;
    push("Cost", format!("${cost:.4}"));
    (title, lines)
}

fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn render_markdown(items: &[Item], meta: Option<&SessionDetail>) -> String {
    let mut out = String::new();
    if let Some(d) = meta {
        let (title, lines) = meta_lines(d);
        let _ = writeln!(out, "# {title}\n");
        for l in lines {
            let _ = writeln!(out, "- **{}:** {}", l.label, l.value);
        }
        out.push('\n');
    }
    for item in items {
        match item {
            Item::Section(role) => {
                let _ = writeln!(out, "## {}\n-----\n", role.label());
            }
            Item::Text(t) => {
                let _ = writeln!(out, "{}\n", t.trim_end());
            }
            Item::Thinking(t) => {
                out.push_str("**Thinking**\n\n");
                for line in t.trim_end().lines() {
                    if line.is_empty() {
                        out.push_str(">\n");
                    } else {
                        let _ = writeln!(out, "> {line}");
                    }
                }
                out.push('\n');
            }
            Item::ToolCall { name, input } => {
                let _ = writeln!(out, "**Tool: {name}**\n");
                fenced(&mut out, "json", input);
            }
            Item::ToolResult {
                name,
                text,
                is_error,
            } => {
                let label = if name.is_empty() { "tool" } else { name };
                let err = if *is_error { " (error)" } else { "" };
                let _ = writeln!(out, "**Result: {label}{err}**\n");
                fenced(&mut out, "", text);
            }
            Item::Attachment { target, is_image } => {
                let file = target.rsplit('/').next().unwrap_or(target);
                if *is_image {
                    let _ = writeln!(out, "![{file}](<{target}>)\n");
                } else {
                    let _ = writeln!(out, "[{file}](<{target}>)\n");
                }
            }
        }
    }
    out.trim_end().to_string() + "\n"
}

fn render_text(items: &[Item], meta: Option<&SessionDetail>) -> String {
    let mut out = String::new();
    if let Some(d) = meta {
        let (title, lines) = meta_lines(d);
        let _ = writeln!(out, "{title}");
        for l in lines {
            let _ = writeln!(out, "{}: {}", l.label, l.value);
        }
        let _ = writeln!(out, "{}\n", "=".repeat(60));
    }
    for item in items {
        match item {
            Item::Section(role) => {
                let label = role.label().to_uppercase();
                let _ = writeln!(out, "{label}\n{}\n", "-".repeat(label.len()));
            }
            Item::Text(t) => {
                let _ = writeln!(out, "{}\n", t.trim_end());
            }
            Item::Thinking(t) => {
                let _ = writeln!(out, "[Thinking]\n{}\n", t.trim_end());
            }
            Item::ToolCall { name, input } => {
                let _ = writeln!(out, "[Tool: {name}]\n{}\n", input.trim_end());
            }
            Item::ToolResult {
                name,
                text,
                is_error,
            } => {
                let label = if name.is_empty() { "tool" } else { name };
                let err = if *is_error { " (error)" } else { "" };
                let _ = writeln!(out, "[Result: {label}{err}]\n{}\n", text.trim_end());
            }
            Item::Attachment { target, .. } => {
                let _ = writeln!(out, "[Attachment: {target}]\n");
            }
        }
    }
    out.trim_end().to_string() + "\n"
}

// --- JSONL ----------------------------------------------------------------

/// A JSON object held as its members in order, each value verbatim.
struct Obj(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Obj {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Obj;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Obj, A::Error> {
                let mut members = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Box<RawValue>>()? {
                    members.push((k, v));
                }
                Ok(Obj(members))
            }
        }
        d.deserialize_map(V)
    }
}

impl Obj {
    fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
    }

    fn set(&mut self, key: &str, value: Box<RawValue>) {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        }
    }

    fn remove(&mut self, key: &str) -> bool {
        let before = self.0.len();
        self.0.retain(|(k, _)| k != key);
        self.0.len() != before
    }

    fn encode(&self) -> String {
        let mut out = String::from("{");
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&serde_json::to_string(k).unwrap_or_default());
            out.push(':');
            out.push_str(v.get());
        }
        out.push('}');
        out
    }
}

fn raw(s: String) -> Option<Box<RawValue>> {
    RawValue::from_string(s).ok()
}

/// The first line JSONL gains when the metadata toggle is on. Its `type` is
/// one no Claude Code event uses, so a consumer can tell it apart.
#[derive(Serialize)]
struct JsonlHeader<'a> {
    #[serde(rename = "type")]
    header_type: &'static str,
    session_id: &'a str,
    title: &'a str,
    project_path: &'a str,
    model: &'a str,
    start_time: String,
    last_activity: String,
    message_count: i64,
    usage: &'a super::summary::TokenUsage,
    cost_usd: f64,
}

fn build_jsonl(lines: &[String], opts: &ExportOptions, meta: Option<&SessionDetail>) -> String {
    let mut out = String::new();
    if let Some(d) = meta {
        let s = &d.summary;
        let header = JsonlHeader {
            header_type: "agento-export",
            session_id: &s.session_id,
            title: &s.display_title,
            project_path: &s.project_path,
            model: &s.model,
            start_time: s.start_time.0.to_rfc3339(),
            last_activity: s.last_activity.0.to_rfc3339(),
            message_count: s.message_count,
            usage: &s.usage,
            cost_usd: s.cost.total_usd + s.subagent_cost.total_usd,
        };
        if let Ok(line) = serde_json::to_string(&header) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    for line in lines {
        if let Some(kept) = jsonl_line(line, opts) {
            out.push_str(&kept);
            out.push('\n');
        }
    }
    out
}

/// One transcript line through the filters: `None` drops it, and a line that
/// needed no edit comes back exactly as it was read.
fn jsonl_line(line: &str, opts: &ExportOptions) -> Option<String> {
    let ev: Event = serde_json::from_str(line).ok()?;
    if ev.is_sidechain == Some(true) {
        return None;
    }
    match ev.event_type.as_deref() {
        Some("system") => return opts.include_system.then(|| line.to_string()),
        Some("user") if is_injected(&ev) => return opts.include_system.then(|| line.to_string()),
        Some("user") | Some("assistant") => {}
        _ => return None,
    }

    let mut obj: Obj = serde_json::from_str(line).ok()?;
    let mut edited = false;
    // Claude Code copies a tool's output onto the carrying event as well as
    // into the block; leaving it would export the results the toggle removed.
    if !opts.include_tool_results && obj.remove("toolUseResult") {
        edited = true;
    }

    let Some(message) = obj.get("message") else {
        return edited
            .then(|| obj.encode())
            .or_else(|| Some(line.to_string()));
    };
    let mut msg: Obj = serde_json::from_str(message.get()).ok()?;
    let Some(content) = msg.get("content") else {
        return edited
            .then(|| obj.encode())
            .or_else(|| Some(line.to_string()));
    };
    // A string content is prose and always stays.
    let Ok(blocks) = serde_json::from_str::<Vec<Box<RawValue>>>(content.get()) else {
        return edited
            .then(|| obj.encode())
            .or_else(|| Some(line.to_string()));
    };

    let total = blocks.len();
    let kept: Vec<&RawValue> = blocks
        .iter()
        .map(|b| b.as_ref())
        .filter(|b| {
            let t = serde_json::from_str::<Block>(b.get())
                .ok()
                .and_then(|b| b.block_type)
                .unwrap_or_default();
            opts.allows(kind_of(&t))
        })
        .collect();
    if kept.is_empty() && total > 0 {
        // Nothing left to say: the event was only what the toggles removed.
        return None;
    }
    if kept.len() == total {
        return edited
            .then(|| obj.encode())
            .or_else(|| Some(line.to_string()));
    }

    let array = format!(
        "[{}]",
        kept.iter().map(|b| b.get()).collect::<Vec<_>>().join(",")
    );
    msg.set("content", raw(array)?);
    obj.set("message", raw(msg.encode())?);
    Some(obj.encode())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(format: Format) -> ExportOptions {
        ExportOptions {
            format,
            include_reasoning: false,
            include_tool_calls: false,
            include_tool_results: false,
            include_system: false,
            include_metadata: false,
        }
    }

    fn all(format: Format) -> ExportOptions {
        ExportOptions {
            format,
            include_reasoning: true,
            include_tool_calls: true,
            include_tool_results: true,
            include_system: true,
            include_metadata: false,
        }
    }

    // "PNG" in base64, so the decoded file is checkable.
    const IMAGE: &str =
        r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"UE5H"}}"#;

    /// Each shape the toggles distinguish, once.
    fn transcript() -> Vec<String> {
        [
            r#"{"type":"file-history-snapshot","messageId":"m0"}"#.to_string(),
            r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"please list files"}}"#.to_string(),
            r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"I should run ls","signature":"x"}]}}"#.to_string(),
            r#"{"type":"assistant","uuid":"a2","message":{"role":"assistant","content":[{"type":"text","text":"Listing now."}]}}"#.to_string(),
            r#"{"type":"assistant","uuid":"a3","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"z":1.50,"command":"ls"}}]}}"#.to_string(),
            r#"{"type":"user","uuid":"u2","toolUseResult":{"stdout":"a.txt"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt\nb.txt"}]}}"#.to_string(),
            format!(r#"{{"type":"user","uuid":"u3","message":{{"role":"user","content":[{{"type":"text","text":"see this"}},{IMAGE}]}}}}"#),
            r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted"}"#.to_string(),
            r#"{"type":"user","uuid":"u4","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>injected</local-command-caveat>"}}"#.to_string(),
            r#"{"type":"user","uuid":"u5","isSidechain":true,"message":{"role":"user","content":"delegated"}}"#.to_string(),
            r#"{"type":"assistant","uuid":"a4","message":{"role":"assistant","content":[{"type":"text","text":"Two files."}]}}"#.to_string(),
        ]
        .into()
    }

    fn md(o: &ExportOptions) -> Built {
        build(&transcript(), o, None, "out-attachments")
    }

    #[test]
    fn markdown_with_everything_off_is_the_conversations_prose() {
        let b = md(&opts(Format::Markdown));
        assert_eq!(
            b.body,
            "## User\n-----\n\nplease list files\n\n\
             ## Assistant\n-----\n\nListing now.\n\n\
             ## User\n-----\n\nsee this\n\n\
             ## Assistant\n-----\n\nTwo files.\n"
        );
        assert!(b.attachments.is_empty());
    }

    #[test]
    fn markdown_renders_each_toggle_as_its_own_block() {
        let b = md(&all(Format::Markdown));
        let body = &b.body;
        assert!(
            body.contains("**Thinking**\n\n> I should run ls\n"),
            "{body}"
        );
        // A labelled block with the input indented in its own order and spelling.
        assert!(
            body.contains(
                "**Tool: Bash**\n\n```json\n{\n  \"z\": 1.50,\n  \"command\": \"ls\"\n}\n```\n"
            ),
            "{body}"
        );
        // The result is named after the call it answers, and belongs to the
        // assistant's section rather than opening a user one.
        assert!(
            body.contains("```\n\n**Result: Bash**\n\n```\na.txt\nb.txt\n```\n"),
            "{body}"
        );
        assert!(
            body.contains("## System\n-----\n\nConversation compacted\n"),
            "{body}"
        );
        assert!(body.contains("<local-command-caveat>injected"), "{body}");
        assert!(!body.contains("delegated"), "sidechain events never export");
        // The image is a file beside the export, linked, never inlined.
        assert!(body.contains("![attachment-001.png](<out-attachments/attachment-001.png>)"));
        assert!(!body.contains("UE5H"));
        assert_eq!(b.attachments.len(), 1);
        assert_eq!(b.attachments[0].name, "attachment-001.png");
        assert_eq!(b.attachments[0].bytes, b"PNG");
    }

    #[test]
    fn each_toggle_changes_the_markdown_on_its_own() {
        let base = md(&opts(Format::Markdown)).body;
        let toggles: [fn(&mut ExportOptions); 4] = [
            |o| o.include_reasoning = true,
            |o| o.include_tool_calls = true,
            |o| o.include_tool_results = true,
            |o| o.include_system = true,
        ];
        for set in toggles {
            let mut o = opts(Format::Markdown);
            set(&mut o);
            assert_ne!(md(&o).body, base, "{o:?} changed nothing");
        }
    }

    #[test]
    fn text_is_a_plain_transcript() {
        let b = md(&all(Format::Text));
        assert!(
            b.body.starts_with("USER\n----\n\nplease list files\n"),
            "{}",
            b.body
        );
        assert!(b.body.contains("[Tool: Bash]\n{\n  \"z\": 1.50,"));
        assert!(b.body.contains("[Result: Bash]\na.txt\nb.txt\n"));
        assert!(b
            .body
            .contains("[Attachment: out-attachments/attachment-001.png]"));
        assert!(!b.body.contains("##") && !b.body.contains("```"));
        assert_eq!(b.attachments.len(), 1);
    }

    #[test]
    fn jsonl_with_everything_off_keeps_prose_events_verbatim() {
        let lines = transcript();
        let b = build(&lines, &opts(Format::Jsonl), None, "x");
        let out: Vec<&str> = b.body.lines().collect();
        assert_eq!(
            out,
            vec![
                lines[1].as_str(),
                lines[3].as_str(),
                // The image is dropped from u3; key order and the rest survive.
                r#"{"type":"user","uuid":"u3","message":{"role":"user","content":[{"type":"text","text":"see this"}]}}"#,
                lines[10].as_str(),
            ]
        );
    }

    #[test]
    fn jsonl_with_everything_on_is_the_stream_minus_non_conversation_events() {
        let lines = transcript();
        let b = build(&lines, &all(Format::Jsonl), None, "x");
        let expected: Vec<&str> = [1, 2, 3, 4, 5, 6, 7, 8, 10]
            .iter()
            .map(|&i| lines[i].as_str())
            .collect();
        assert_eq!(b.body.lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn jsonl_drops_tool_use_result_with_the_results_toggle() {
        let mut o = all(Format::Jsonl);
        o.include_tool_results = false;
        let b = build(&transcript(), &o, None, "x");
        assert!(!b.body.contains("toolUseResult"));
        assert!(
            !b.body.contains("\"u2\""),
            "an event of only results is dropped"
        );
        // The number keeps its spelling through a re-assembled line too.
        assert!(b.body.contains(r#""input":{"z":1.50,"command":"ls"}"#));
    }

    #[test]
    fn a_session_with_nothing_optional_exports_cleanly_with_every_toggle_on() {
        let lines = vec![
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#.to_string(),
        ];
        for f in [Format::Markdown, Format::Text, Format::Jsonl] {
            let b = build(&lines, &all(f), None, "x");
            assert!(b.body.contains("hello"));
            assert!(b.attachments.is_empty());
        }
    }

    #[test]
    fn a_fence_outgrows_the_backticks_inside_it() {
        assert_eq!(fence("plain"), "```");
        assert_eq!(fence("has ``` inside"), "````");
    }

    #[test]
    fn pretty_json_keeps_strings_and_empty_containers_intact() {
        assert_eq!(
            pretty_json(r#"{"a":"x,{y}:","b":[],"c":{}}"#),
            "{\n  \"a\": \"x,{y}:\",\n  \"b\": [],\n  \"c\": {}\n}"
        );
    }

    /// The command's filesystem half: the attachment lands in the sibling
    /// folder before the file that links it.
    #[test]
    fn export_writes_attachments_beside_the_file() {
        let corpus = tempfile::tempdir().expect("corpus");
        let project = corpus.path().join("projects").join("-p");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(project.join("s1.jsonl"), transcript().join("\n")).expect("transcript");

        let data = tempfile::tempdir().expect("data");
        let db = data.path().join("agento.db");
        let mut conn = crate::native::db::ensure_database(&db).expect("db");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute("INSERT OR IGNORE INTO user_settings (id) VALUES (1)", [])
            .expect("settings row");
        conn.execute(
            "UPDATE user_settings SET claude_config_dirs = ?1 WHERE id = 1",
            [serde_json::to_string(&[corpus.path().to_string_lossy()]).unwrap()],
        )
        .expect("settings");
        drop(conn);

        let out = tempfile::tempdir().expect("out");
        let dest = out.path().join("my session.md");
        let mut o = all(Format::Markdown);
        o.include_metadata = true;
        let r = export(&db, "s1", &dest, &o).expect("export");
        assert_eq!(r.attachments, 1);
        let written = std::fs::read_to_string(&dest).expect("file");
        assert!(written.starts_with("# "), "{written}");
        assert!(written.contains("- **Session:** s1"));
        assert!(written.contains("(<my session-attachments/attachment-001.png>)"));
        assert_eq!(
            std::fs::read(out.path().join("my session-attachments/attachment-001.png")).unwrap(),
            b"PNG"
        );

        assert!(export(&db, "missing", &dest, &o).is_err());
    }
}
