//! skilld 的端到端门禁：起真实进程，走 SEP-2640 的四个方法。
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn temp_skills() -> PathBuf {
    let root = std::env::temp_dir().join(format!("skilld-test-{}", std::process::id()));
    let demo = root.join("demo");
    std::fs::create_dir_all(demo.join("references")).unwrap();
    std::fs::write(
        demo.join("SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\n# demo\n",
    )
    .unwrap();
    std::fs::write(demo.join("references/a.md"), "A\n").unwrap();
    root
}

struct Session {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Session {
    fn start(dir: &PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_skilld"))
            .arg("--dir")
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn skilld");
        let reader = BufReader::new(child.stdout.take().unwrap());
        Self { child, reader }
    }

    fn send(&mut self, payload: serde_json::Value) -> serde_json::Value {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{payload}").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "skilld 提前退出");
            let value: serde_json::Value = serde_json::from_str(line.trim()).expect("JSON 行");
            if value.get("id").is_some() {
                return value;
            }
        }
    }

    fn request(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        let mut payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method});
        if !params.is_null() {
            payload["params"] = params;
        }
        self.send(payload)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn serves_skills_with_digests_and_no_list_caching() {
    let dir = temp_skills();
    let mut session = Session::start(&dir);

    let init = session.request(
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }),
    );
    let capabilities = &init["result"]["capabilities"];
    assert_eq!(
        capabilities["extensions"]["io.modelcontextprotocol/skills"]["directoryRead"],
        serde_json::json!(true)
    );

    let list = session.request(2, "skills/list", serde_json::Value::Null);
    assert_eq!(list["result"]["ttlMs"], serde_json::json!(0));
    assert_eq!(list["result"]["cacheScope"], serde_json::json!("private"));
    let skills = list["result"]["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["uri"], "skill://demo/SKILL.md");
    let resources = skills[0]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2);
    for resource in resources {
        let digest = resource["digest"].as_str().unwrap();
        assert!(
            digest.starts_with("sha256:") && digest.len() == 71,
            "{digest}"
        );
        assert!(resource["size"].as_u64().unwrap() > 0);
    }

    let read = session.request(
        3,
        "resources/read",
        serde_json::json!({"uri": "skill://demo/SKILL.md"}),
    );
    assert!(
        read["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("name: demo")
    );

    let missing = session.request(
        4,
        "skills/get",
        serde_json::json!({"uri": "skill://nope/SKILL.md"}),
    );
    assert_eq!(missing["error"]["code"], serde_json::json!(-32602));

    std::fs::remove_dir_all(&dir).unwrap();
}
