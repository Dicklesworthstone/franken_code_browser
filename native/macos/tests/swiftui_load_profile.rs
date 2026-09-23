//! Opt-in measurement of actual native source/highlight/shape/raster work.
#![cfg(target_os = "macos")]
use std::{path::PathBuf, process::Command};

fn checked(command: &mut Command) {
    eprintln!("profile command: {command:?}");
    assert!(command.status().expect("run profile command").success());
}

#[test]
#[ignore = "requires explicit immutable corpus and native bridge; measures real loading"]
fn profile_native_loading() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bridge = std::env::var("FCB_BRIDGE_LIB").expect("explicit verified archive");
    let corpus = std::env::var("FCB_PROFILE_CORPUS").expect("explicit immutable real corpus");
    let cached = std::env::var("FCB_PROFILE_MODE").as_deref() == Ok("cached");
    let scratch = std::env::temp_dir().join(format!("fcb-load-profile-{}", std::process::id()));
    std::fs::create_dir(&scratch).expect("private new profile directory");
    let binary = scratch.join("AtlasLoadProfile");
    let oracle = scratch.join("oracle.json");
    if let Some(expected) = std::env::var_os("FCB_PROFILE_ORACLE") {
        std::fs::copy(expected, &oracle).expect("retain preexisting source/pixel oracle");
    } else {
        assert!(
            !cached,
            "cache measurement requires baseline FCB_PROFILE_ORACLE"
        );
    }
    let cache = std::env::var_os("FCB_PROFILE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| scratch.join("project-cache"));
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-O",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasProjectCache.swift",
                "swiftui/tests/AtlasLoadProfile.swift",
            ])
            .arg(&bridge)
            .arg("-o")
            .arg(&binary),
    );
    let run = |passes: &str| {
        let mut cmd = Command::new("/usr/bin/time");
        cmd.arg("-l")
            .arg(&binary)
            .arg(&corpus)
            .arg(&oracle)
            .arg(passes);
        if cached {
            cmd.arg(&cache);
        }
        cmd
    };
    eprintln!("FIRST OBSERVED launch; OS page-cache state unknown");
    checked(&mut run("1"));
    eprintln!("SAME PROCESS repeats; cached mode retains the actual project cache owner");
    checked(&mut run("2"));
    if std::env::var("FCB_PROFILE_REOPEN_ONLY").as_deref() == Ok("1") {
        eprintln!(
            "REOPEN_DIAGNOSTIC_ONLY: cold and reopened/RAM assertions completed; no hyperfine series requested"
        );
        return;
    }
    let quoted = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut command = format!(
        "{} {} {} 1",
        quoted(binary.to_str().unwrap()),
        quoted(&corpus),
        quoted(oracle.to_str().unwrap())
    );
    if cached {
        command.push_str(&format!(" {}", quoted(cache.to_str().unwrap())));
    }
    let results = scratch.join("hyperfine.json");
    checked(
        Command::new("hyperfine")
            .args([
                "--warmup",
                "3",
                "--runs",
                "10",
                "--show-output",
                "--export-json",
            ])
            .arg(&results)
            .arg(command),
    );
    eprintln!(
        "HYPERFINE_JSON {}",
        std::fs::read_to_string(results).unwrap()
    );
    eprintln!("SOURCE_ORACLE {}", std::fs::read_to_string(oracle).unwrap());
}
