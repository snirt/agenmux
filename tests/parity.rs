// Regex-dialect parity: every bash fixture must detect the same state via the
// Rust engine. Fixture name: <agent>-<state>[-n].txt, optional .title sidecar.
use std::path::Path;
use std::process::Command;

#[test]
fn fixtures_match_expected_state() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bin = env!("CARGO_BIN_EXE_agents-mon");
    let mut checked = 0;
    for entry in std::fs::read_dir(root.join("tests/fixtures")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "txt") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy();
        let fixture = stem.as_ref();
        let stem = match fixture.rsplit_once('-') {
            Some((name, variant))
                if !variant.is_empty() && variant.bytes().all(|b| b.is_ascii_digit()) =>
            {
                name
            }
            _ => fixture,
        };
        let (agent, expected) = stem.rsplit_once('-').unwrap();
        let conf = root.join(format!("agents/{agent}.conf"));
        let title = std::fs::read_to_string(path.with_extension("title"))
            .map(|t| t.trim_end().to_string())
            .unwrap_or_default();
        let out = Command::new(bin)
            .arg("detect")
            .arg(&conf)
            .arg(&path)
            .arg(&title)
            .output()
            .unwrap();
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(got, expected, "fixture {fixture}");
        checked += 1;
    }
    assert!(checked >= 13, "only {checked} fixtures found");
}
