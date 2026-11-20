//! `wfscan mcp`: a Model Context Protocol server on stdin and stdout, so an AI client
//! (Claude Code, Claude Desktop, ...) can look through a cache and export from it.
//!
//! The protocol is JSON-RPC 2.0, one message per line (MCP's stdio transport). Only
//! what a tool server needs is here: `initialize`, `ping`, `tools/list` and
//! `tools/call`. Everything else gets "method not found"; notifications are ignored.
//! Nothing but protocol messages goes to stdout: notes go to stderr.
//!
//! The cache is opened and indexed (or the manifest given with `-m` loaded) when a tool
//! first needs it, and kept for the rest of the session. Like the rest of wfscan it only
//! reads the cache; `export` writes only under the folder it is given.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use wfscan_core::export::check_manifest;
use wfscan_core::{
    AudioOutput, Cache, Classifier, ExportOptions, Exporters, FileEntry, FormatOptions, IndexOptions, Kind, Manifest, Oodle, OodleDecoder,
    Part, Status, TextureOutput, VideoOutput, export, index, package_info, report, with_input,
};

/// The protocol version this server speaks, and the older ones it also accepts.
const PROTOCOL: &str = "2025-06-18";
const ACCEPTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Most bytes `read_bytes` returns at once.
const MAX_READ: u64 = 64 * 1024;
/// Most files `find` lists at once.
const MAX_FIND: u64 = 1000;

const INSTRUCTIONS: &str = "wfscan reads Warframe's Cache.Windows (read-only). Every file has an internal path \
(such as /Lotus/Characters/Tenno/Excalibur/ExcaliburBody_d.png), lives in a package (Misc, Texture, ...) and has up to \
three parts: H (header: a common header with the type number and attribute text, then the type's own header), \
B and F (data; textures keep small mips in B and large ones in F). Start with `find` to locate files, `file` for one \
file's details, `read_bytes` / `find_bytes` to look at raw decompressed data, `report` for what wfscan can and can't \
export, and `export` to write models (glTF), textures (DDS/PNG) and sounds (Ogg/Opus/WAV) to a folder.";

struct State {
    cache: Cache,
    manifest: Manifest,
}

pub struct Server {
    cache_dir: PathBuf,
    /// Loaded at the start if it matches the cache; saved after each export.
    manifest_path: Option<PathBuf>,
    oodle: Result<Oodle, String>,
    /// Given with `--bink-dll`; loaded the first time an export asks for PNG frames.
    bink_dll: Option<PathBuf>,
    bink: Option<std::sync::Arc<wfscan_core::BinkLibrary>>,
    allow_in_use: bool,
    state: Option<State>,
}

impl Server {
    pub fn new(cache_dir: PathBuf, manifest_path: Option<PathBuf>, oodle: Result<Oodle, String>, bink_dll: Option<PathBuf>, allow_in_use: bool) -> Self {
        Self { cache_dir, manifest_path, oodle, bink_dll, bink: None, allow_in_use, state: None }
    }

    /// Answer messages from `input` on `output` until `input` ends.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line.context("reading stdin")?;
            // Some shells start piped text with a byte-order mark.
            let line = line.trim_start_matches('\u{feff}');
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(line) {
                serde_json::to_writer(&mut output, &reply)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// The reply to one message, or `None` for a notification or a response.
    pub fn handle(&mut self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(e) => return Some(error_reply(Value::Null, -32700, &format!("not JSON: {e}"))),
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to something we never send, or not a request at all.
            return message.get("id").filter(|_| message.get("result").is_none() && message.get("error").is_none()).map(|id| {
                error_reply(id.clone(), -32600, "not a JSON-RPC request")
            });
        };
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
                let version = if ACCEPTED.contains(&asked) { asked } else { PROTOCOL };
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "wfscan", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error_reply(id, -32602, "tools/call needs a tool name"));
                };
                if !tools().iter().any(|t| t["name"] == name) {
                    return Some(error_reply(id, -32602, &format!("no tool named {name}")));
                }
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match self.call(name, &args) {
                    Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
                    Err(e) => json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true }),
                }
            }
            other => return Some(error_reply(id, -32601, &format!("method {other} is not supported"))),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn decoder(&self) -> Option<&dyn OodleDecoder> {
        self.oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder)
    }

    /// Open and index the cache, or load the manifest, the first time it is needed.
    fn open(&mut self) -> Result<()> {
        if self.state.is_some() {
            return Ok(());
        }
        let cache = Cache::open(&self.cache_dir)?;
        if !self.allow_in_use {
            cache.check_not_in_use()?;
        }
        let saved = match &self.manifest_path {
            Some(path) if path.exists() => {
                let manifest = Manifest::load(path)?;
                match check_manifest(&cache, &manifest) {
                    Ok(()) => Some(manifest),
                    Err(e) => {
                        eprintln!("wfscan mcp: {} doesn't match the cache ({e}); indexing again", path.display());
                        None
                    }
                }
            }
            _ => None,
        };
        let manifest = match saved {
            Some(manifest) => manifest,
            None => {
                let (manifest, stats) = index(&cache, self.decoder(), &IndexOptions::default())?;
                if let (Err(load_error), true) = (&self.oodle, stats.need_oodle > 0) {
                    bail!("{} files have Oodle blocks and couldn't be read: {load_error} (start the server with --oodle-dll PATH)", stats.need_oodle);
                }
                eprintln!("wfscan mcp: indexed {} files in {:.1} s", stats.files, stats.elapsed.as_secs_f64());
                manifest
            }
        };
        self.state = Some(State { cache, manifest });
        Ok(())
    }

    fn state(&mut self) -> Result<&mut State> {
        self.open()?;
        Ok(self.state.as_mut().expect("opened above"))
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<String> {
        let value = match name {
            "info" => self.info()?,
            "find" => self.find(args)?,
            "file" => self.file(args)?,
            "read_bytes" => return self.read_bytes(args),
            "find_bytes" => self.find_bytes(args)?,
            "export" => self.export(args)?,
            "report" => self.report(args)?,
            _ => unreachable!("checked against tools()"),
        };
        Ok(serde_json::to_string_pretty(&value)?)
    }

    fn info(&mut self) -> Result<Value> {
        let state = self.state()?;
        let packages: Vec<_> = state.cache.packages.iter().map(package_info).collect();
        let live = state.manifest.files.iter().filter(|f| !f.stale).count();
        Ok(json!({
            "dir": state.cache.dir.display().to_string(),
            "game": state.cache.game,
            "files": live,
            "stale": state.manifest.files.len() - live,
            "packages": packages,
        }))
    }

    fn find(&mut self, args: &Value) -> Result<Value> {
        let text = opt_str(args, "text")?.map(str::to_ascii_lowercase);
        let prefix = opt_str(args, "prefix")?;
        let package = opt_str(args, "package")?;
        let kind = opt_str(args, "kind")?.map(parse_kind).transpose()?;
        let type_id = opt_u64(args, "type")?;
        let status = opt_str(args, "status")?.map(parse_status).transpose()?;
        let stale = opt_bool(args, "include_stale")?.unwrap_or(false);
        let limit = opt_u64(args, "limit")?.unwrap_or(100).clamp(1, MAX_FIND) as usize;
        let offset = opt_u64(args, "offset")?.unwrap_or(0) as usize;

        let state = self.state()?;
        let classifier = state.manifest.classifier();
        let matches: Vec<&FileEntry> = state
            .manifest
            .files
            .iter()
            .filter(|f| stale || !f.stale)
            .filter(|f| package.is_none_or(|p| f.package == p))
            .filter(|f| prefix.is_none_or(|p| f.path.starts_with(p)))
            .filter(|f| text.as_ref().is_none_or(|t| f.path.to_ascii_lowercase().contains(t.as_str())))
            .filter(|f| type_id.is_none_or(|t| f.header.as_ref().is_some_and(|h| u64::from(h.type_id) == t)))
            .filter(|f| kind.is_none_or(|k| classifier.kind(f) == Some(k)))
            .filter(|f| status.is_none_or(|s| f.export.as_ref().is_some_and(|e| e.status == s)))
            .collect();
        let files: Vec<Value> = matches.iter().skip(offset).take(limit).map(|f| summary(f, &classifier)).collect();
        Ok(json!({ "total": matches.len(), "offset": offset, "shown": files.len(), "files": files }))
    }

    fn file(&mut self, args: &Value) -> Result<Value> {
        let path = req_str(args, "path")?.to_string();
        let package = opt_str(args, "package")?.map(str::to_string);
        self.open()?;
        let decoder = self.oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder);
        let state = self.state.as_ref().expect("opened above");
        let entry = &state.manifest.files[locate(&state.manifest, &path, package.as_deref())?];
        let classifier = state.manifest.classifier();
        let header = with_input(&state.cache, &entry.package, &entry.path, decoder, |input| {
            json!({
                "type": input.header.type_id,
                "type_flags": input.header.type_flags,
                "header_bytes": input.header.len,
                "sources": input.header.sources,
                "attributes": input.header.attributes,
            })
        });
        Ok(json!({
            "package": entry.package,
            "category": classifier.category(entry).name(),
            "path": entry.path,
            "kind": classifier.kind(entry).map(Kind::name),
            "stale": entry.stale,
            "h": entry.h,
            "b": entry.b,
            "f": entry.f,
            "common_header": match header {
                Ok(header) => header,
                Err(e) => json!({ "error": e.to_string() }),
            },
            "read_error": entry.error,
            "export": entry.export,
        }))
    }

    /// One part of a file, decompressed and whole.
    fn read_part(&mut self, args: &Value) -> Result<(String, Part, Vec<u8>)> {
        let path = req_str(args, "path")?.to_string();
        let package = opt_str(args, "package")?.map(str::to_string);
        let part = parse_part(opt_str(args, "part")?.unwrap_or("H"))?;
        self.open()?;
        let decoder = self.oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder);
        let state = self.state.as_ref().expect("opened above");
        let entry = &state.manifest.files[locate(&state.manifest, &path, package.as_deref())?];
        let source = state.cache.package(&entry.package).ok_or_else(|| anyhow!("no package named {}", entry.package))?;
        let toc = source
            .part(part)
            .and_then(|file| Some((file, file.find(&entry.path)?)))
            .ok_or_else(|| anyhow!("{} has no {part} part", entry.path))?;
        let (data, _) = toc.0.read(toc.1, decoder).map_err(|e| match (&e, &self.oodle) {
            (wfscan_core::Error::Block { source: wfscan_core::BlockError::NoOodle { .. }, .. }, Err(load_error)) => {
                anyhow!("{e}: {load_error} (start the server with --oodle-dll PATH)")
            }
            _ => e.into(),
        })?;
        Ok((format!("{} {}", entry.package, entry.path), part, data))
    }

    fn read_bytes(&mut self, args: &Value) -> Result<String> {
        let offset = opt_u64(args, "offset")?.unwrap_or(0);
        let length = opt_u64(args, "length")?.unwrap_or(256).clamp(1, MAX_READ);
        let text = match opt_str(args, "format")?.unwrap_or("hex") {
            "hex" => false,
            "text" => true,
            other => bail!("unknown format {other:?} (hex or text)"),
        };
        let (name, part, data) = self.read_part(args)?;
        let start = (offset as usize).min(data.len());
        let end = (start + length as usize).min(data.len());
        let bytes = &data[start..end];
        let mut out = format!("{name}, part {part}: {} bytes; showing {start}..{end}\n", data.len());
        if text {
            out.push_str(&String::from_utf8_lossy(bytes));
        } else {
            out.push_str(&hex_dump(bytes, start));
        }
        Ok(out)
    }

    fn find_bytes(&mut self, args: &Value) -> Result<Value> {
        let pattern = parse_hex(req_str(args, "pattern")?)?;
        let limit = opt_u64(args, "limit")?.unwrap_or(50).clamp(1, 1000) as usize;
        let (name, part, data) = self.read_part(args)?;
        let offsets: Vec<usize> = data.windows(pattern.len()).enumerate().filter(|(_, w)| *w == pattern.as_slice()).map(|(i, _)| i).collect();
        Ok(json!({
            "file": name,
            "part": part,
            "part_size": data.len(),
            "matches": offsets.len(),
            "offsets": offsets.iter().take(limit).collect::<Vec<_>>(),
        }))
    }

    fn export(&mut self, args: &Value) -> Result<Value> {
        let out_dir = PathBuf::from(req_str(args, "out_dir")?);
        let paths = opt_str_list(args, "paths")?;
        let prefix = opt_str(args, "prefix")?.map(str::to_string);
        let package = opt_str(args, "package")?.map(str::to_string);
        let kinds = opt_str_list(args, "kinds")?.unwrap_or_default().iter().map(|k| parse_kind(k)).collect::<Result<Vec<_>>>()?;
        let all = opt_bool(args, "all")?.unwrap_or(false);
        let texture: TextureOutput = opt_str(args, "texture_format")?.unwrap_or("dds").parse().map_err(|e| anyhow!("{e}"))?;
        let audio: AudioOutput = opt_str(args, "audio_format")?.unwrap_or("native").parse().map_err(|e| anyhow!("{e}"))?;
        let video: VideoOutput = opt_str(args, "video_format")?.unwrap_or("bk2").parse().map_err(|e| anyhow!("{e}"))?;
        if video == VideoOutput::Png && self.bink.is_none() {
            self.bink = Some(std::sync::Arc::new(crate::load_bink(self.bink_dll.as_deref(), &self.cache_dir)?));
        }
        let bink = if video == VideoOutput::Png { self.bink.clone() } else { None };
        let raw_unsupported = opt_bool(args, "raw_unsupported")?.unwrap_or(false);
        let dry_run = opt_bool(args, "dry_run")?.unwrap_or(false);
        let force = opt_bool(args, "force")?.unwrap_or(false);
        if !all && paths.is_none() && prefix.is_none() && package.is_none() && kinds.is_empty() {
            bail!("say what to export (paths, prefix, package or kinds), or pass all: true for the whole cache");
        }
        if !out_dir.is_absolute() {
            bail!("out_dir must be an absolute path");
        }

        self.open()?;
        let decoder = self.oodle.as_ref().ok().map(|o| o as &dyn OodleDecoder);
        let manifest_path = self.manifest_path.clone();
        let state = self.state.as_mut().expect("opened above");
        let wanted: Option<HashSet<&str>> = paths.as_ref().map(|p| p.iter().map(String::as_str).collect());
        let classifier = state.manifest.classifier();
        let selected: Vec<usize> = state
            .manifest
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.stale)
            .filter(|(_, f)| wanted.as_ref().is_none_or(|w| w.contains(f.path.as_str())))
            .filter(|(_, f)| package.as_ref().is_none_or(|p| *p == f.package))
            .filter(|(_, f)| prefix.as_ref().is_none_or(|p| f.path.starts_with(p.as_str())))
            .filter(|(_, f)| kinds.is_empty() || classifier.kind(f).is_some_and(|k| kinds.contains(&k)))
            .map(|(i, _)| i)
            .collect();
        drop(classifier);
        if let Some(wanted) = &wanted {
            let found: HashSet<&str> = selected.iter().map(|&i| state.manifest.files[i].path.as_str()).collect();
            let missing: Vec<&&str> = wanted.iter().filter(|p| !found.contains(**p)).collect();
            if !missing.is_empty() {
                bail!("no such file (or not matching the other filters): {missing:?}");
            }
        }
        if selected.is_empty() {
            bail!("nothing matches");
        }

        let exporters = Exporters::builtin(&FormatOptions { texture, audio, vorbis: None, video, bink });
        let opts = ExportOptions { files: Some(selected.iter().copied().collect()), dry_run, force, raw_unsupported, ..Default::default() };
        // A dry run works on a copy, so its records don't stay behind.
        let mut copy;
        let manifest = if dry_run {
            copy = state.manifest.clone();
            &mut copy
        } else {
            &mut state.manifest
        };
        let stats = export(&state.cache, manifest, None, &exporters, decoder, &out_dir, &opts)?;
        // Failures first, then the rest, at most 100.
        let mut records: Vec<Value> = selected
            .iter()
            .map(|&i| &manifest.files[i])
            .filter_map(|f| Some((f, f.export.as_ref()?)))
            .map(|(f, e)| json!({ "package": f.package, "path": f.path, "status": e.status.name(), "exporter": e.exporter, "reason": e.reason, "outputs": e.outputs }))
            .collect();
        records.sort_by_key(|r| r["status"] != "failed");
        records.truncate(100);
        let mut saved = None;
        if !dry_run && let Some(path) = &manifest_path {
            state.manifest.save(path)?;
            saved = Some(path.display().to_string());
        }
        Ok(json!({ "out_dir": out_dir.display().to_string(), "dry_run": dry_run, "stats": stats, "manifest_saved": saved, "files": records }))
    }

    fn report(&mut self, args: &Value) -> Result<Value> {
        let max_errors = opt_u64(args, "errors")?.unwrap_or(20) as usize;
        let unknown = opt_bool(args, "unknown")?.unwrap_or(false);
        let state = self.state()?;
        let mut report = report(&state.manifest);
        if unknown {
            report.types.retain(|t| t.kind.is_none());
        }
        report.errors.truncate(max_errors);
        Ok(serde_json::to_value(&report)?)
    }
}

/// The tools, with their input schemas.
fn tools() -> Vec<Value> {
    let path_args = json!({
        "path": { "type": "string", "description": "Internal path, such as /Lotus/Sounds/Tone.wav" },
        "package": { "type": "string", "description": "Package name; needed only when several packages have the path" },
    });
    let with = |extra: Value| {
        let mut props = path_args.as_object().unwrap().clone();
        props.extend(extra.as_object().unwrap().clone());
        Value::Object(props)
    };
    let kinds = "model, texture, material, audio, level, level-static, landscape, shader, icon, video, flash, script, sound-bank";
    vec![
        json!({
            "name": "info",
            "description": "The cache: game, packages with file counts and sizes.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "find",
            "description": "Search files by path and type. Returns package, path, kind, type number, parts (H/B/F), size and export status of each, with paging.",
            "inputSchema": { "type": "object", "properties": {
                "text": { "type": "string", "description": "Part of the path, any case" },
                "prefix": { "type": "string", "description": "Path starts with this (case matters), such as /Lotus/Characters/" },
                "package": { "type": "string" },
                "kind": { "type": "string", "description": kinds },
                "type": { "type": "integer", "description": "Type number from the common header" },
                "status": { "type": "string", "description": "Export status: ok, failed, unsupported, skipped" },
                "include_stale": { "type": "boolean", "description": "Also deleted or replaced files the cache still lists" },
                "limit": { "type": "integer", "description": "At most this many (default 100, up to 1000)" },
                "offset": { "type": "integer", "description": "Skip this many matches first" },
            } },
        }),
        json!({
            "name": "file",
            "description": "One file's details: package, kind, H/B/F offsets and sizes, the common header (type, source paths, attribute text), read error and last export.",
            "inputSchema": { "type": "object", "properties": path_args.clone(), "required": ["path"] },
        }),
        json!({
            "name": "read_bytes",
            "description": "Raw bytes of one part (H, B or F) of a file, decompressed, as a hex dump or text. The H part starts with the common header.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "part": { "type": "string", "enum": ["H", "B", "F"], "description": "Default H" },
                "offset": { "type": "integer", "description": "Default 0" },
                "length": { "type": "integer", "description": "Default 256, at most 65536" },
                "format": { "type": "string", "enum": ["hex", "text"], "description": "Default hex" },
            })), "required": ["path"] },
        }),
        json!({
            "name": "find_bytes",
            "description": "Where a byte pattern occurs in one part of a file. Returns the offsets.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "part": { "type": "string", "enum": ["H", "B", "F"], "description": "Default H" },
                "pattern": { "type": "string", "description": "Hex bytes, spaces allowed, such as \"4E C6 67 18\"" },
                "limit": { "type": "integer", "description": "At most this many offsets (default 50)" },
            })), "required": ["path", "pattern"] },
        }),
        json!({
            "name": "export",
            "description": "Export files into a folder: models as glTF (.glb), textures as DDS or PNG, sounds as Ogg/Opus/WAV. Every file gets a status (ok, failed, unsupported, skipped). Choose files by paths, prefix, package and/or kinds; all: true exports the whole cache (over 100 GiB). Use dry_run to see what would happen.",
            "inputSchema": { "type": "object", "properties": {
                "out_dir": { "type": "string", "description": "Absolute folder to write into" },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Exact internal paths" },
                "prefix": { "type": "string", "description": "Every file whose path starts with this" },
                "package": { "type": "string" },
                "kinds": { "type": "array", "items": { "type": "string" }, "description": kinds },
                "all": { "type": "boolean" },
                "texture_format": { "type": "string", "enum": ["dds", "png"], "description": "Default dds" },
                "audio_format": { "type": "string", "enum": ["native", "wav"], "description": "Default native" },
                "video_format": { "type": "string", "enum": ["bk2", "png"], "description": "Default bk2; png writes every frame (needs the Bink library: the game's Tools/Bink2/bink2w64.dll is found next to the cache)" },
                "raw_unsupported": { "type": "boolean", "description": "Write files nothing can convert as their raw parts (PATH.H, .B, .F)" },
                "dry_run": { "type": "boolean", "description": "Work out the results but write nothing" },
                "force": { "type": "boolean", "description": "Export again even where an earlier export still stands" },
            }, "required": ["out_dir"] },
        }),
        json!({
            "name": "report",
            "description": "Coverage per file type: how many files, whether wfscan knows the type, how exports ended, and the most common failures.",
            "inputSchema": { "type": "object", "properties": {
                "errors": { "type": "integer", "description": "How many kinds of failure to list (default 20)" },
                "unknown": { "type": "boolean", "description": "Only types wfscan has no name for" },
            } },
        }),
    ]
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn summary(f: &FileEntry, classifier: &Classifier) -> Value {
    let parts: String = Part::ALL.iter().map(|&p| if f.part(p).is_some() { p.letter() } else { '-' }).collect();
    json!({
        "package": f.package,
        "path": f.path,
        "kind": classifier.kind(f).map(Kind::name),
        "type": f.header.as_ref().map(|h| h.type_id),
        "parts": parts,
        "size": f.size(),
        "stale": f.stale,
        "status": f.export.as_ref().map(|e| e.status.name()),
        "error": f.error,
    })
}

/// The live entry at `path`, in `package` if given.
fn locate(manifest: &Manifest, path: &str, package: Option<&str>) -> Result<usize> {
    let found: Vec<usize> =
        manifest.files.iter().enumerate().filter(|(_, f)| !f.stale && f.path == path && package.is_none_or(|p| f.package == p)).map(|(i, _)| i).collect();
    match found.as_slice() {
        [] => bail!("no file at {path}{} (use `find` to search)", package.map(|p| format!(" in {p}")).unwrap_or_default()),
        [one] => Ok(*one),
        several => {
            let packages: Vec<&str> = several.iter().map(|&i| manifest.files[i].package.as_str()).collect();
            bail!("{path} is in several packages ({}); pass package", packages.join(", "))
        }
    }
}

/// Classic hex dump lines: offset, 16 bytes, their ASCII.
fn hex_dump(bytes: &[u8], start: usize) -> String {
    let mut out = String::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }).collect();
        out.push_str(&format!("{:08x}  {:<47}  {ascii}\n", start + row * 16, hex.join(" ")));
    }
    out
}

fn parse_hex(text: &str) -> Result<Vec<u8>> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let digits = digits.strip_prefix("0x").unwrap_or(&digits);
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        bail!("pattern must be whole hex bytes, such as \"4E C6 67 18\"");
    }
    (0..digits.len()).step_by(2).map(|i| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|_| anyhow!("{:?} is not hex", &digits[i..i + 2]))).collect()
}

fn parse_part(text: &str) -> Result<Part> {
    Part::ALL.into_iter().find(|p| p.letter().eq_ignore_ascii_case(&text.chars().next().unwrap_or(' ')) && text.len() == 1).ok_or_else(|| anyhow!("part must be H, B or F"))
}

fn parse_kind(text: &str) -> Result<Kind> {
    text.parse().map_err(|e| anyhow!("{e}"))
}

fn parse_status(text: &str) -> Result<Status> {
    Status::ALL.into_iter().find(|s| s.name() == text).ok_or_else(|| anyhow!("unknown status {text:?} (ok, failed, unsupported or skipped)"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => bail!("`{key}` must be a string"),
    }
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(args, key)?.ok_or_else(|| anyhow!("`{key}` is required"))
}

fn opt_u64(args: &Value, key: &str) -> Result<Option<u64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| anyhow!("`{key}` must be a whole number")),
    }
}

fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| anyhow!("`{key}` must be true or false")),
    }
}

fn opt_str_list(args: &Value, key: &str) -> Result<Option<Vec<String>>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items.iter().map(|v| v.as_str().map(str::to_string).ok_or_else(|| anyhow!("`{key}` must be a list of strings"))).collect::<Result<_>>().map(Some),
        Some(_) => bail!("`{key}` must be a list of strings"),
    }
}
