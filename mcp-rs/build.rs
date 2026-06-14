//! Build script: read `[package.metadata.learn]` from Cargo.toml and
//! expose as `LEARN_*` env vars for `env!()` in source code.
//!
//! This allows tuning learning parameters without recompiling Rust code —
//! just edit Cargo.toml and rebuild.

use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let toml_path = Path::new(&manifest_dir).join("Cargo.toml");
    let content = std::fs::read_to_string(&toml_path).expect("Cannot read Cargo.toml");

    let config: toml::Value = toml::from_str(&content).expect("Cannot parse Cargo.toml");

    let learn = config
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("learn"));

    if let Some(learn) = learn {
        emit_if_present(learn, "cycles", "LEARN_CYCLES");
        emit_if_present(learn, "compare_lines", "LEARN_COMPARE_LINES");
        emit_if_present(learn, "similarity_threshold", "LEARN_SIMILARITY_THRESHOLD");
        emit_if_present(
            learn,
            "relay_broken_threshold",
            "LEARN_RELAY_BROKEN_THRESHOLD",
        );
        emit_if_present(learn, "reset_pulse_ms", "LEARN_RESET_PULSE_MS");
        emit_if_present(learn, "capture_timeout_secs", "LEARN_CAPTURE_TIMEOUT_SECS");
        emit_if_present(learn, "learner_stage_threshold", "LEARN_STAGE_THRESHOLD");
        emit_if_present(learn, "learner_crash_threshold", "LEARN_CRASH_THRESHOLD");
        emit_if_present(learn, "crash_patterns", "LEARN_CRASH_PATTERNS");
    } else {
        // Defaults if [package.metadata.learn] is missing
        println!("cargo:rustc-env=LEARN_CYCLES=3");
        println!("cargo:rustc-env=LEARN_COMPARE_LINES=50");
        println!("cargo:rustc-env=LEARN_SIMILARITY_THRESHOLD=0.93");
        println!("cargo:rustc-env=LEARN_RELAY_BROKEN_THRESHOLD=0.10");
        println!("cargo:rustc-env=LEARN_RESET_PULSE_MS=500");
        println!("cargo:rustc-env=LEARN_CAPTURE_TIMEOUT_SECS=30.0");
        println!("cargo:rustc-env=LEARN_STAGE_THRESHOLD=0.45");
        println!("cargo:rustc-env=LEARN_CRASH_THRESHOLD=0.50");
    }
}

fn emit_if_present(table: &toml::Value, key: &str, env_name: &str) {
    if let Some(val) = table.get(key) {
        match val {
            toml::Value::Integer(i) => println!("cargo:rustc-env={env_name}={i}"),
            toml::Value::Float(f) => println!("cargo:rustc-env={env_name}={f}"),
            toml::Value::String(s) => println!("cargo:rustc-env={env_name}={s}"),
            toml::Value::Array(arr) => {
                // Join string array elements with newlines (for crash_patterns etc.)
                let joined: String = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !joined.is_empty() {
                    println!("cargo:rustc-env={env_name}={joined}");
                }
            }
            _ => {} // skip unsupported types
        }
    }
}
