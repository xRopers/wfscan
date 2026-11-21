//! `wfscan mcp` driven as an MCP client would: JSON-RPC lines on its stdin and stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use wfscan_fixtures::{self as fx, snd};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wfscan"))
            .arg("mcp")
            .args(args)
            .env_remove("WFSCAN_OODLE_DLL")
            .env_remove("ZSCAN_OODLE_DLL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start wfscan mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout, next_id: 1 }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let reply = self.receive();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// Call a tool; returns its text and whether it is an error.
    fn call(&mut self, tool: &str, arguments: Value) -> (String, bool) {
        let reply = self.request("tools/call", json!({ "name": tool, "arguments": arguments }));
        let result = &reply["result"];
        (result["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{reply}")).to_string(), result["isError"].as_bool().unwrap())
    }

    /// Call a tool that answers in JSON, and expect it to succeed.
    fn call_json(&mut self, tool: &str, arguments: Value) -> Value {
        let (text, error) = self.call(tool, arguments);
        assert!(!error, "{tool} failed: {text}");
        serde_json::from_str(&text).unwrap()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn cache() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::sounds().without_oodle().write(&dir).unwrap();
    (tmp, dir.to_str().unwrap().to_string())
}

#[test]
fn handshake_and_tool_list() {
    let (_tmp, cache) = cache();
    let mut c = Client::start(&[&cache]);
    let init = c.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "wfscan");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    // A notification gets no reply: the next line is the ping's.
    c.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));

    // An unknown version is answered with ours.
    assert_eq!(c.request("initialize", json!({ "protocolVersion": "1999-01-01" }))["result"]["protocolVersion"], "2025-06-18");

    let tools = c.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["info", "find", "file", "read_bytes", "find_bytes", "export", "report", "survey"]);
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
    }

    // Protocol errors.
    assert_eq!(c.request("resources/list", json!({}))["error"]["code"], -32601);
    assert_eq!(c.request("tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
    c.send(&json!("not a request"));
    c.stdin.write_all(b"{ this is not json\n").unwrap();
    c.stdin.flush().unwrap();
    assert_eq!(c.receive()["error"]["code"], -32700);
}

#[test]
fn find_file_and_bytes() {
    let (_tmp, cache) = cache();
    let mut c = Client::start(&[&cache]);

    let info = c.call_json("info", json!({}));
    assert_eq!(info["game"], "warframe");

    let found = c.call_json("find", json!({ "text": "sounds/", "kind": "audio" }));
    let paths: Vec<&str> = found["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&snd::TONE) && paths.contains(&snd::SPLIT), "{paths:?}");
    assert_eq!(found["total"].as_u64().unwrap() as usize, paths.len());
    let paged = c.call_json("find", json!({ "text": "sounds/", "limit": 2, "offset": 1 }));
    assert_eq!((paged["shown"].as_u64(), paged["files"][0]["path"].as_str()), (Some(2), Some(paths[1])));

    let file = c.call_json("file", json!({ "path": snd::TONE }));
    assert_eq!((file["kind"].as_str(), file["common_header"]["type"].as_u64()), (Some("audio"), Some(139)));
    assert!(file["common_header"]["attributes"].as_str().unwrap().contains("CompressionHint"));
    let b_size = file["b"]["size"].as_u64().unwrap();

    // The fixture puts five 0xee bytes before the samples in B.
    let (dump, error) = c.call("read_bytes", json!({ "path": snd::TONE, "part": "B", "length": 16 }));
    assert!(!error, "{dump}");
    assert!(dump.contains(&format!("{b_size} bytes; showing 0..16")) && dump.contains("00000000  ee ee ee ee ee "), "{dump}");
    let (text, _) = c.call("read_bytes", json!({ "path": snd::TONE, "part": "H", "format": "text", "length": 4096 }));
    assert!(text.contains("CompressionHint"), "{text}");
    let hits = c.call_json("find_bytes", json!({ "path": snd::TONE, "part": "b", "pattern": "ee ee ee ee ee" }));
    assert_eq!(hits["offsets"], json!([0]));

    // Mistakes come back as tool errors, not protocol errors.
    let (message, error) = c.call("file", json!({ "path": "/No/Such/File" }));
    assert!(error && message.contains("no file at /No/Such/File"), "{message}");
    let (message, error) = c.call("read_bytes", json!({ "path": snd::TONE, "part": "F" }));
    assert!(error && message.contains("no F part"), "{message}");
    let (message, error) = c.call("find_bytes", json!({ "path": snd::TONE, "pattern": "e" }));
    assert!(error && message.contains("whole hex bytes"), "{message}");
    let (message, error) = c.call("find", json!({ "kind": "mesh" }));
    assert!(error && message.contains("mesh"), "{message}");
}

#[test]
fn export_writes_and_saves_the_manifest() {
    let (tmp, cache) = cache();
    let manifest = tmp.path().join("m.json");
    let out = tmp.path().join("out");
    let mut c = Client::start(&[&cache, "-m", manifest.to_str().unwrap()]);
    let out_dir = out.to_str().unwrap();

    let (message, error) = c.call("export", json!({ "out_dir": out_dir }));
    assert!(error && message.contains("all: true"), "{message}");
    let (message, error) = c.call("export", json!({ "out_dir": "relative/dir", "paths": [snd::TONE] }));
    assert!(error && message.contains("absolute"), "{message}");
    let (message, error) = c.call("export", json!({ "out_dir": out_dir, "paths": ["/No/Such.wav"] }));
    assert!(error && message.contains("/No/Such.wav"), "{message}");

    // A dry run writes nothing and leaves no records behind.
    let dry = c.call_json("export", json!({ "out_dir": out_dir, "paths": [snd::TONE], "dry_run": true }));
    assert_eq!((dry["stats"]["ok"].as_u64(), dry["files"][0]["status"].as_str()), (Some(1), Some("ok")));
    assert!(!out.exists() && !manifest.exists());
    assert_eq!(c.call_json("find", json!({ "status": "ok" }))["total"], 0);

    let done = c.call_json("export", json!({ "out_dir": out_dir, "prefix": "/Lotus/Sounds/", "raw_unsupported": true }));
    let stats = &done["stats"];
    assert_eq!((stats["ok"].as_u64(), stats["raw"].as_u64(), stats["failed"].as_u64()), (Some(2), Some(2), Some(1)), "{done}");
    assert!(out.join("Lotus").join("Sounds").join("Tone.wav").is_file());
    assert!(out.join("Lotus").join("Sounds").join("Old.wav.B").is_file(), "ADPCM has no reader: written raw");
    assert_eq!(done["manifest_saved"], manifest.to_str().unwrap());
    assert!(manifest.is_file());

    // A second server picks the manifest up, records and all.
    drop(c);
    let mut c = Client::start(&[&cache, "-m", manifest.to_str().unwrap()]);
    let ok = c.call_json("find", json!({ "status": "ok" }));
    assert_eq!(ok["total"], stats["ok"]);
    let report = c.call_json("report", json!({}));
    assert!(report["types"].as_array().unwrap().iter().any(|t| t["kind"] == "audio" && t["ok"].as_u64() == stats["ok"].as_u64()), "{report}");

    // A survey of the sounds' type: every one of them, the smallest listed.
    let sounds = c.call_json("find", json!({ "type": 139, "limit": 1 }));
    let survey = c.call_json("survey", json!({ "type": 139, "package": "Misc", "samples": 2 }));
    assert_eq!((&survey["files"], &survey["unreadable"]), (&sounds["total"], &json!(0)), "{survey}");
    assert_eq!(survey["samples"].as_array().unwrap().len(), 2);
    assert!(survey["h"]["min"].as_u64().unwrap() > 0);
    let (message, error) = c.call("survey", json!({}));
    assert!(error && message.contains("`type` is required"), "{message}");
}

#[test]
fn a_cache_that_needs_oodle_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::basic().write(&dir).unwrap();
    let mut c = Client::start(&[dir.to_str().unwrap()]);
    // Finding files reads only H parts, which aren't Oodle here; reading a B part is.
    let found = c.call_json("find", json!({}));
    let with_b: Vec<String> =
        found["files"].as_array().unwrap().iter().filter(|f| f["parts"].as_str().unwrap().contains('B')).map(|f| f["path"].as_str().unwrap().to_string()).collect();
    let errors: Vec<String> = with_b.iter().map(|path| c.call("read_bytes", json!({ "path": path, "part": "B" }))).filter(|(_, error)| *error).map(|(m, _)| m).collect();
    assert!(!errors.is_empty(), "some B part needs Oodle");
    assert!(errors.iter().all(|m| m.contains("--oodle-dll")), "{errors:?}");
}
