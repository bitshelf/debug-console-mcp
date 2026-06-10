use super::*;

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|x| x.to_string()).collect()
}

/// b1: the `dutabo agent` arg parse — bare/`--select` → Tui;
/// `deploy …` → the flag struct (--target/--item selectors, schema
/// v1); unknown words → Unknown; a bare flag (no subcommand) →
/// Unknown.
#[test]
fn agent_arg_parse() {
    assert_eq!(parse_agent_args(&s(&[])).unwrap(), AgentInvocation::Tui);
    assert_eq!(
        parse_agent_args(&s(&["--select"])).unwrap(),
        AgentInvocation::Tui
    );
    // 3-level selectors.
    let deploy = parse_agent_args(&s(&[
        "deploy",
        "--agent",
        "a",
        "--target",
        "Rockchip",
        "--item",
        "AOSP",
        "--project",
        "p",
        "--offline",
        "--force",
    ]))
    .unwrap();
    assert_eq!(
        deploy,
        AgentInvocation::Deploy(AgentDeployArgs {
            agent: Some("a".into()),
            target: Some("Rockchip".into()),
            item: Some("AOSP".into()),
            project: Some("p".into()),
            offline: true,
            force: true,
        })
    );
    // 2-level selector (no --item).
    let deploy = parse_agent_args(&s(&["deploy", "--agent", "a", "--target", "Rust"])).unwrap();
    assert_eq!(
        deploy,
        AgentInvocation::Deploy(AgentDeployArgs {
            agent: Some("a".into()),
            target: Some("Rust".into()),
            item: None,
            project: None,
            offline: false,
            force: false,
        })
    );
    // A missing value is a parse error naming the flag.
    let err = parse_agent_args(&s(&["deploy", "--target"])).unwrap_err();
    assert!(err.contains("--target"), "{err}");
    // A value-starting-with-dash is refused as a value.
    let err = parse_agent_args(&s(&["deploy", "--agent", "--x"])).unwrap_err();
    assert!(err.contains("--agent"), "{err}");
    assert_eq!(
        parse_agent_args(&s(&["bogus"])).unwrap(),
        AgentInvocation::Unknown("bogus".into())
    );
    assert_eq!(
        parse_agent_args(&s(&["status"])).unwrap(),
        AgentInvocation::Unknown("status".into())
    );
    // A bare flag without a subcommand is not a known word.
    assert_eq!(
        parse_agent_args(&s(&["--agent", "x"])).unwrap(),
        AgentInvocation::Unknown("--agent".into())
    );
    // An unknown deploy flag is a hard parse error naming it.
    let err = parse_agent_args(&s(&["deploy", "--nope"])).unwrap_err();
    assert!(err.contains("--nope"), "{err}");
}

/// b3: the schema-v1 resolver — exact 2/3-level paths, prefix
/// filters, ambiguity and not-found errors naming the paths.
#[test]
fn agent_resolve_v1_paths() {
    let text = r#"{
  "version": 1,
  "agents": {
    "claude-code": {
      "deploy": { "dst": ".claude", "mode": "sync" },
      "targets": {
        "Rockchip": { "items": {
          "AOSP": { "src": "https://x/rk-aosp.git" },
          "Linux": { "src": "https://x/rk-linux.git" }
        } },
        "Rust": { "src": "https://x/rust.git" }
      }
    },
    "dsh": {
      "deploy": { "dst": "", "mode": "merge" },
      "targets": { "Linux": { "src": "https://x/dsh-linux.git" } }
    }
  }
}
"#;
    let catalog = sermcp::agent_catalog::AgentCatalog::parse(text).unwrap();
    // Exact 3-level.
    let e = resolve_agent_entry(
        &catalog,
        Some("claude-code"),
        Some("Rockchip"),
        Some("AOSP"),
    )
    .unwrap();
    assert_eq!(e.path(), "claude-code/Rockchip/AOSP");
    assert_eq!(e.src, "https://x/rk-aosp.git");
    // Exact 2-level.
    let e = resolve_agent_entry(&catalog, Some("dsh"), Some("Linux"), None).unwrap();
    assert_eq!(e.path(), "dsh/Linux");
    // Prefix filters narrow to the unique match (--target matches the
    // GROUP tier for 3-level paths, the leaf for 2-level paths).
    let e = resolve_agent_entry(&catalog, Some("dsh"), None, None).unwrap();
    assert_eq!(e.path(), "dsh/Linux");
    let e = resolve_agent_entry(&catalog, None, Some("Rust"), None).unwrap();
    assert_eq!(e.path(), "claude-code/Rust");
    let e = resolve_agent_entry(&catalog, Some("claude-code"), None, Some("AOSP")).unwrap();
    assert_eq!(e.path(), "claude-code/Rockchip/AOSP");
    // A group name alone is ambiguous across its items.
    let err = resolve_agent_entry(&catalog, None, Some("Rockchip"), None).unwrap_err();
    assert!(err.contains("ambiguous"), "{err}");
    // Ambiguity lists the candidates (agent-only prefix over 3 paths).
    let err = resolve_agent_entry(&catalog, Some("claude-code"), None, None).unwrap_err();
    assert!(err.contains("ambiguous"), "{err}");
    assert!(err.contains("claude-code/Rockchip/AOSP"), "{err}");
    assert!(err.contains("claude-code/Rockchip/Linux"), "{err}");
    assert!(err.contains("claude-code/Rust"), "{err}");
    // Not found names the full selector path.
    let err = resolve_agent_entry(
        &catalog,
        Some("claude-code"),
        Some("Rockchip"),
        Some("nope"),
    )
    .unwrap_err();
    assert!(err.contains("claude-code/Rockchip/nope"), "{err}");
    // An --item on a 2-level leaf is not found (groups only).
    let err = resolve_agent_entry(&catalog, Some("dsh"), Some("Linux"), Some("x")).unwrap_err();
    assert!(err.contains("dsh/Linux/x"), "{err}");
    // No match at all.
    let err = resolve_agent_entry(&catalog, Some("nope"), None, None).unwrap_err();
    assert!(err.contains("no catalog entry"), "{err}");
}

/// b2: `--project` wins; env parents next; cwd last (the flag/cwd
/// tiers; the env tiers share the t10 ENV_MUTEX coverage).
#[test]
fn resolve_project_dir_precedence() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        agent_tui::resolve_project_dir(Some("/flag/proj")),
        std::path::PathBuf::from("/flag/proj"),
        "flag wins"
    );
    unsafe {
        std::env::remove_var("DUTABO_TARGET_CONF");
        std::env::remove_var("TARGET_CONF");
    }
    assert_eq!(
        agent_tui::resolve_project_dir(None),
        std::env::current_dir().unwrap(),
        "cwd fallback"
    );
    unsafe {
        std::env::set_var("TARGET_CONF", "/tgt/y/.target.jsonc");
    }
    assert_eq!(
        agent_tui::resolve_project_dir(None),
        std::path::PathBuf::from("/tgt/y"),
        "TARGET_CONF parent"
    );
    unsafe {
        std::env::remove_var("TARGET_CONF");
        std::env::set_var("DUTABO_TARGET_CONF", "/dut/x/.target.jsonc");
    }
    assert_eq!(
        agent_tui::resolve_project_dir(None),
        std::path::PathBuf::from("/dut/x"),
        "DUTABO_TARGET_CONF parent"
    );
    unsafe {
        std::env::remove_var("DUTABO_TARGET_CONF");
    }
}
