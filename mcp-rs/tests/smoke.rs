//! Smoke tests — verify basic functionality works end-to-end.
//!
//! These tests validate that the MCP server can start, tools are properly
//! defined, and config validation catches common errors. They run without
//! hardware (no serial port needed).

use debug_console_mcp::config::Config;
use debug_console_mcp::mcp::McpServer;
use std::collections::HashMap;

/// Verify tool definitions are valid and have the expected structure.
#[test]
fn test_all_tools_have_valid_schema() {
    let server = create_test_server();
    assert!(server.tools.len() >= 28, "Expected at least 28 MCP tools, got {}", server.tools.len());

    for tool in &server.tools {
        assert!(!tool.name.is_empty(), "Tool has empty name");
        assert!(!tool.description.is_empty(), "Tool '{}' has empty description", tool.name);
        assert!(tool.input_schema.is_object(), "Tool '{}' input_schema is not an object", tool.name);
    }
}

/// Verify essential tools exist.
#[test]
fn test_essential_tools_present() {
    let server = create_test_server();
    let names: Vec<&str> = server.tools.iter().map(|t| t.name).collect();

    let required = &[
        "serial_send_command",
        "serial_get_state",
        "serial_get_logs",
        "serial_reset",
        "serial_enter_uboot",
        "serial_enter_maskrom",
        "serial_wait_pattern",
        "serial_uboot_command",
        "serial_flash",
        "serial_flash_plan",
        "serial_get_config",
        "serial_load_reference",
        "serial_learn_connection",
    ];

    for &r in required {
        assert!(names.contains(&r), "Essential tool '{}' is missing", r);
    }
}

/// Verify config validation catches missing host IP.
#[test]
fn test_config_validation_missing_host() {
    let mut values = HashMap::new();
    values.insert("SERIAL_PORT".into(), "2002".into());
    values.insert("DUT_DIR".into(), ".dut-serial".into());
    // DEV_HOST_IP is intentionally empty

    let cfg = Config {
        values,
        config_path: None,
        project_dir: None,
        format: debug_console_mcp::config::ConfigFormat::None,
    };

    let result = cfg.validate();
    assert!(result.is_err(), "Should error when dev_host_ip is empty");
    let errors = result.unwrap_err();
    assert!(errors.iter().any(|e| e.contains("dev_host")),
        "Error should mention dev_host, got: {:?}", errors);
}

/// Verify config validation catches missing serial port.
#[test]
fn test_config_validation_missing_port() {
    let mut values = HashMap::new();
    values.insert("DEV_HOST_IP".into(), "192.168.1.1".into());
    values.insert("SERIAL_PORT".into(), "0".into());
    values.insert("DUT_DIR".into(), ".dut-serial".into());

    let cfg = Config {
        values,
        config_path: None,
        project_dir: None,
        format: debug_console_mcp::config::ConfigFormat::None,
    };

    let result = cfg.validate();
    assert!(result.is_err(), "Should error when serial_port is 0");
    let errors = result.unwrap_err();
    assert!(errors.iter().any(|e| e.contains("serial.port") || e.contains("port")),
        "Error should mention port, got: {:?}", errors);
}

/// Verify strip-ansi-escapes works on prompt-like data.
#[test]
fn test_strip_ansi_escapes_on_prompt() {
    let input = b"\x1b[?2004hroot@board:/# ls\r\n";
    let clean = strip_ansi_escapes::strip(input);
    let text = String::from_utf8_lossy(&clean);
    assert!(!text.contains("\x1b"), "ANSI escapes should be stripped, got: {:?}", text);
    assert!(text.contains("root@board"), "Prompt text should be preserved");
    assert!(text.contains("/#"), "Sigil should be preserved");
}

/// Verify highlight module works.
#[test]
fn test_highlight_prompt() {
    let mut out = Vec::new();
    debug_console_mcp::highlight::highlight_serial_prompt(b"root@board:/# ls\r\n", &mut out);
    let rendered = String::from_utf8(out).unwrap();
    assert!(rendered.contains("\x1b[1;32mroot@board\x1b[0m"), "user should be green");
    assert!(rendered.contains("\x1b[1;34m/\x1b[0m"), "dir should be blue");
    assert!(rendered.contains("\x1b[1;37m# \x1b[0m"), "sigil should be white");
}

/// Helper: create a McpServer for testing (no serial connection).
fn create_test_server() -> McpServer {
    let mut values = HashMap::new();
    values.insert("DEV_HOST_IP".into(), "127.0.0.1".into());
    values.insert("SERIAL_PORT".into(), "59999".into());
    values.insert("RELAY_PORT".into(), "0".into());
    values.insert("RESET_CHANNEL".into(), "0".into());
    values.insert("MASKROM_CHANNEL".into(), "0".into());
    values.insert("HANG_TIMEOUT".into(), "60".into());
    values.insert("HANG_HYSTERESIS".into(), "3".into());
    values.insert("MAX_ARCHIVED_LOGS".into(), "10".into());
    values.insert("MAX_LOG_FILE_SIZE".into(), "100".into());
    values.insert("DUT_DIR".into(), ".dut-serial".into());
    values.insert("LOCK_DIR".into(), "/tmp/debug-console-test-locks".into());
    values.insert("LOGIN_USER".into(), "root".into());
    values.insert("LOGIN_PASS".into(), "".into());
    values.insert("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into());

    let config = Config {
        values,
        config_path: None,
        project_dir: None,
        format: debug_console_mcp::config::ConfigFormat::None,
    };

    let engine = debug_console_mcp::serial_engine::new_shared_engine(config);
    McpServer::new(engine, debug_console_mcp::task_manager::TaskManager::new())
}
