use super::*;
use ratatui::backend::TestBackend;
use sermcp::init::{InitOptions, build_form_schema};
use std::collections::VecDeque;
use std::sync::MutexGuard;

/// Test EventSource: pops synthetic events from a queue (keys AND mouse).
pub struct KeyQueue {
    pub events: VecDeque<Event>,
}

impl KeyQueue {
    pub fn new(events: Vec<Event>) -> Self {
        Self {
            events: events.into(),
        }
    }
}

impl EventSource for KeyQueue {
    fn next_event(&mut self) -> io::Result<Event> {
        self.events
            .pop_front()
            .ok_or_else(|| io::Error::other("no more events"))
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

fn chr(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn enter() -> KeyEvent {
    key(KeyCode::Enter)
}

fn esc() -> KeyEvent {
    key(KeyCode::Esc)
}

fn tab() -> KeyEvent {
    key(KeyCode::Tab)
}

fn down() -> KeyEvent {
    key(KeyCode::Down)
}

fn f2() -> KeyEvent {
    key(KeyCode::F(2))
}

fn del() -> KeyEvent {
    key(KeyCode::Delete)
}

/// A KeyEvent as an Event (for the KeyQueue).
fn ev(key: KeyEvent) -> Event {
    Event::Key(key)
}

/// Chars of `s` as key Events (the form has no per-field Enter — typed
/// chars land in the focused editor directly).
fn typed(s: &str) -> Vec<Event> {
    s.chars().map(|c| ev(chr(c))).collect()
}

fn mouse_down(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mouse_drag(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mouse_up(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mouse_move(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Moved,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn create_schema(opts: &InitOptions) -> FormSchema {
    build_form_schema(None, opts)
}

fn create_state(opts: &InitOptions) -> FormState {
    FormState::new(create_schema(opts), "/proj/.target.jsonc".into())
}

fn quick() -> InitOptions {
    InitOptions {
        quick: true,
        ..Default::default()
    }
}

fn terminal_with(state: &mut FormState, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_form(f, state)).unwrap();
    terminal
}

fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buf = terminal.backend().buffer();
    buf.content
        .chunks(buf.area.width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect())
        .collect()
}

/// run_init touches TARGET_CONF — serialize with every other env
/// mutator in this bin process (the dutabo/agent_tui tests lock the
/// same crate::ENV_MUTEX; a per-module mutex cannot protect the
/// process-global environ).
fn lock_env() -> MutexGuard<'static, ()> {
    crate::ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}

fn test_tui() -> FormTui<TestBackend, KeyQueue> {
    FormTui::new(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&quick()),
    )
}

/// Type `text` into a field's editor (focus moves to that field).
fn type_into(state: &mut FormState, key: FieldKey, text: &str) {
    state.focus = FocusSlot::Field(key);
    for c in text.chars() {
        handle_form_key(state, chr(c));
    }
}

// ── Form state model ───────────────────────────────────────────────

/// CREATE starts empty (only the relay gate seeded NO), focused on the
/// first host tab with its first DUT selected, and dim auto-name hints.
#[test]
fn test_form_state_new_create_starts_empty_with_hints() {
    let state = create_state(&quick());
    assert_eq!(state.host_count, 1);
    assert_eq!(state.dut_counts, vec![1]);
    assert_eq!(
        state.values,
        BTreeMap::from([(FieldKey::DutRelayGate(0, 0), "no".to_string())]),
        "{:?}",
        state.values
    );
    assert_eq!(state.focus, FocusSlot::HostTab(0));
    assert_eq!(state.selected_host, 0);
    assert_eq!(state.selected_dut, Some(0));
    assert!(!state.gate);
    assert!(state.other_expanded, "advanced expanded by default");
    // Dim auto-name hints (DUT names only — hosts have no name).
    let dut = state
        .fields
        .iter()
        .find(|f| f.key == FieldKey::DutName(0, 0))
        .unwrap();
    assert_eq!(dut.default.as_deref(), Some("dut1"));
}

#[test]
fn test_form_state_new_update_prefills_every_existing_index() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "host_name": "lab-pc",
      "user": "root",
      "pass": "",
      "duts": [
        {
          "dut_name": "dut1",
          "serial": { "port": 41002 },
          "relay": { "type": "" }
        }
      ]
    }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let state = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    assert_eq!(
        state.values.get(&FieldKey::HostCount).map(String::as_str),
        Some("1")
    );
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("10.0.0.1")
    );
    // The host_name key prefills the optional name buffer verbatim.
    assert_eq!(
        state.values.get(&FieldKey::HostName(0)).map(String::as_str),
        Some("lab-pc")
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutPort(0, 0))
            .map(String::as_str),
        Some("41002")
    );
    // The empty relay.type prefill IS the current value "" — an
    // all-default F2 save must keep it (never fabricate "ch340").
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some("")
    );
}

#[test]
fn test_test_snapshot_uses_unsaved_form_values() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let saved = r#"{
  "dev_hosts": [{
    "ip": "192.168.1.105",
    "user": "linaro",
    "duts": [{
      "dut_name": "rk3576",
      "serial": { "port": 2002, "baudrate": 115200 },
      "control": { "dev_ctrl": "ch340-relay" },
      "relay": { "port": 2000, "reset_ch": 1 }
    }]
  }]
}"#;
    std::fs::write(tmp.path().join(".target.jsonc"), saved).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let mut state = FormState::new(
        prepared.schema.clone(),
        prepared.target_path.display().to_string(),
    );
    state.set_value(FieldKey::HostIp(0), "192.168.1.106".into());
    state.set_value(FieldKey::DutRelayPort(0, 0), "2004".into());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "serial.baudrate"),
        "11520".into(),
    );
    let answers = resolve_answers(
        &state.schema,
        &state.submit_values(),
        state.gate,
        NamePolicy::ErrorOnCollision,
    )
    .unwrap();
    let snapshot = init::render_test_config(&prepared, &answers).unwrap();
    let parsed = sermcp::config::parse_jsonc_text(&snapshot).unwrap();
    let host = &parsed.dev_hosts[0];
    let dut = &host.duts[0];
    assert_eq!(host.ip.as_deref(), Some("192.168.1.106"));
    assert_eq!(
        dut.relay
            .as_ref()
            .and_then(|relay| relay.port.as_ref())
            .map(ToString::to_string)
            .as_deref(),
        Some("2004")
    );
    assert_eq!(
        dut.serial
            .as_ref()
            .and_then(|serial| serial.baudrate.as_ref())
            .map(NumberOrString::to_value_string)
            .as_deref(),
        Some("11520")
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(".target.jsonc")).unwrap(),
        saved,
        "TEST snapshot must not modify the saved config"
    );
}

#[test]
fn test_probe_dependency_graph_blocks_invalid_upstream_fields() {
    use std::collections::HashSet;
    let keys = HashSet::from([
        FieldKey::HostIp(0),
        FieldKey::HostUser(0),
        FieldKey::HostPass(0),
        FieldKey::DutPort(0, 0),
        FieldKey::DutRelayPort(0, 0),
        FieldKey::DutRelayReset(0, 0),
        FieldKey::DutAdvanced(0, 0, "serial.baudrate"),
        FieldKey::DutAdvanced(0, 0, "monitor.reference_log"),
    ]);
    let reference = ProbeTarget::RelayResetCycle {
        ports: vec![3000],
        reference_log_path: "reference.log".into(),
        use_power: false,
        #[cfg(feature = "direct-dut-probe")]
        direct: DirectResetCycleProbe {
            relay_host: "127.0.0.1".into(),
            relay_port: 2000,
            channel: 1,
            serial_host: "127.0.0.1".into(),
            serial_port: 2002,
            save_path: "reference.log".into(),
            reset_hold_ms: 10,
        },
    };
    let dependencies = probe_dependencies(
        &FieldKey::DutAdvanced(0, 0, "monitor.reference_log"),
        &reference,
        &keys,
    );
    assert!(dependencies.contains(&FieldKey::HostIp(0)));
    assert!(!dependencies.contains(&FieldKey::HostUser(0)));
    assert!(!dependencies.contains(&FieldKey::HostPass(0)));
    assert!(dependencies.contains(&FieldKey::DutRelayReset(0, 0)));
    assert!(dependencies.contains(&FieldKey::DutAdvanced(0, 0, "serial.baudrate")));

    let reset = probe_dependencies(
        &FieldKey::DutRelayReset(0, 0),
        &ProbeTarget::McpNormalizeState {
            ports: vec![3000],
            use_power: false,
            mode_buttons: Vec::new(),
            linked_keys: Vec::new(),
        },
        &keys,
    );
    assert!(reset.contains(&FieldKey::HostIp(0)));
    assert!(!reset.contains(&FieldKey::HostUser(0)));
    assert!(!reset.contains(&FieldKey::HostPass(0)));
    assert!(!reset.contains(&FieldKey::DutRelayPort(0, 0)));

    let flash = probe_dependencies(
        &FieldKey::DutAdvanced(0, 0, "flash.list_devices_cmd"),
        &ProbeTarget::FlashListPreflight {
            host: "127.0.0.1".into(),
            user: "user".into(),
            pass: "pass".into(),
            tool: "upgrade_tool".into(),
            list_cmd: "ld".into(),
        },
        &keys,
    );
    assert!(flash.contains(&FieldKey::HostIp(0)));
    assert!(flash.contains(&FieldKey::HostUser(0)));
    assert!(flash.contains(&FieldKey::HostPass(0)));
}

#[test]
fn test_password_probe_links_credential_result_onto_user() {
    let user = FieldKey::HostUser(0);
    let target = ProbeTarget::SshPasswordLogin {
        host: "127.0.0.1".into(),
        user: "user".into(),
        pass: "wrong".into(),
        linked_user: user.clone(),
    };
    assert_eq!(linked_probe_keys(&target), vec![user]);
}

#[test]
fn test_ssh_key_success_proves_user_but_not_password() {
    let result = ssh_credential_execution(ProbeMark::Ok, ProbeMark::Err);
    assert_eq!(result.mark, ProbeMark::Err, "password field must fail");
    assert_eq!(
        result.linked_mark,
        Some(ProbeMark::Ok),
        "the same username already authenticated with the key"
    );
    assert_eq!(result.detail.as_deref(), Some("SSH key: ✓; password: ✗"));

    let result = ssh_credential_execution(ProbeMark::Err, ProbeMark::Err);
    assert_eq!(result.mark, ProbeMark::Err);
    assert_eq!(result.linked_mark, Some(ProbeMark::Err));
    assert!(
        result
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("cannot distinguish"))
    );
}

#[test]
fn test_auth_probe_detail_is_visible_on_the_credential_row() {
    let mut state = create_state(&quick());
    let user = FieldKey::HostUser(0);
    let pass = FieldKey::HostPass(0);
    state.values.insert(pass.clone(), "configured".into());
    state
        .probe_details
        .insert(pass.clone(), "SSH key: ✓; password: ✗".into());
    assert!(visible_auth_probe_detail(&state, &user).is_none());
    assert_eq!(
        visible_auth_probe_detail(&state, &pass),
        Some("SSH key: ✓; password: ✗")
    );

    state.values.insert(pass, String::new());
    state
        .probe_details
        .insert(user.clone(), "SSH key: ✓".into());
    assert_eq!(visible_auth_probe_detail(&state, &user), Some("SSH key: ✓"));
}

#[test]
fn test_wrong_dev_host_blocks_dut_relay_and_reference_calls() {
    let host_key = FieldKey::HostIp(0);
    let relay_port_key = FieldKey::DutRelayPort(0, 0);
    let reset_key = FieldKey::DutRelayReset(0, 0);
    let baud_key = FieldKey::DutAdvanced(0, 0, "serial.baudrate");
    let reference_key = FieldKey::DutAdvanced(0, 0, "monitor.reference_log");
    let probes = vec![
        (
            host_key.clone(),
            ProbeTarget::SshBanner {
                host: "127.0.0.1".into(),
                port: 0,
            },
        ),
        (
            relay_port_key.clone(),
            ProbeTarget::TcpConnect {
                host: "127.0.0.1".into(),
                port: 0,
            },
        ),
        (
            reset_key.clone(),
            ProbeTarget::McpNormalizeState {
                ports: vec![0],
                use_power: false,
                mode_buttons: Vec::new(),
                linked_keys: Vec::new(),
            },
        ),
        (baud_key.clone(), ProbeTarget::AlwaysFail),
        (
            reference_key.clone(),
            ProbeTarget::RelayResetCycle {
                ports: vec![0],
                reference_log_path: "unused".into(),
                use_power: false,
                #[cfg(feature = "direct-dut-probe")]
                direct: DirectResetCycleProbe {
                    relay_host: "127.0.0.1".into(),
                    relay_port: 0,
                    channel: 1,
                    serial_host: "127.0.0.1".into(),
                    serial_port: 0,
                    save_path: "unused".into(),
                    reset_hold_ms: 1,
                },
            },
        ),
    ];
    let (tx, rx) = std::sync::mpsc::channel();
    run_probes(probes, tx);
    let results: std::collections::HashMap<_, _> = rx
        .try_iter()
        .filter(|(_, mark, _)| *mark != ProbeMark::Pending)
        .map(|(key, mark, detail)| (key, (mark, detail)))
        .collect();
    assert_eq!(
        results.get(&host_key).map(|result| result.0),
        Some(ProbeMark::Err)
    );
    for key in [relay_port_key, reset_key, baud_key, reference_key] {
        let (mark, detail) = results.get(&key).expect("dependent result");
        assert_eq!(*mark, ProbeMark::Skip, "{key:?} must be blocked");
        assert!(
            detail
                .as_deref()
                .is_some_and(|text| text.contains("blocked")),
            "{key:?} must explain the upstream failure"
        );
    }
}

/// Per-DUT advanced buffers: typing into dut2's crash_keywords leaves
/// dut1's buffer untouched; a DUT removal renumbers the surviving
/// DUT's typed value with it (migration).
#[test]
fn test_dut_advanced_buffers_are_per_dut() {
    let mut state = create_state(&quick());
    state.add_dut(0);
    type_into(
        &mut state,
        FieldKey::DutAdvanced(0, 1, "monitor.crash_keywords"),
        "Oops2",
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutAdvanced(0, 1, "monitor.crash_keywords"))
            .map(String::as_str),
        Some("Oops2")
    );
    assert!(
        !state
            .values
            .contains_key(&FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords")),
        "dut1's buffer must stay untouched: {:?}",
        state.values
    );
    // Focus landing on dut2's advanced field selects dut2.
    state.set_focus(FocusSlot::Field(FieldKey::DutAdvanced(
        0,
        1,
        "monitor.crash_keywords",
    )));
    assert_eq!(state.selected_dut, Some(1));
    // Removing dut1 renumbers dut2's typed buffer into slot 0.
    state.focus = FocusSlot::Button(ButtonId::RemoveDut(0, 0));
    handle_form_key(&mut state, chr(' '));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords"))
            .map(String::as_str),
        Some("Oops2"),
        "typed per-DUT advanced values follow their DUT row"
    );
}

/// F2 with the advanced gate OFF (collapsed [Other]) persists typed
/// DUT-owned values into the DUT's override sections; the global
/// sections stay untouched (GATE BUG fix).
#[test]
fn test_f2_gate_off_persists_dut_owned_values_into_file() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "root",
      "pass": "",
      "duts": [
        { "dut_name": "dut1", "serial": { "port": 41002, "baudrate": 460800 } }
      ]
    }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &InitOptions::default()).unwrap();
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        FormState::new(
            prepared.schema.clone(),
            prepared.target_path.display().to_string(),
        ),
    );
    tui.write_ctx = Some((prepared, InitOptions::default()));
    // Collapse [Other] — Confirm mode: the gate turns OFF.
    tui.state.focus = FocusSlot::Button(ButtonId::ToggleOther);
    handle_form_key(&mut tui.state, enter());
    assert!(!tui.state.gate);
    // Type DUT-owned values into the DUT box while collapsed.
    type_into(
        &mut tui.state,
        FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords"),
        "Oops:",
    );
    tui.state.set_value(
        FieldKey::DutAdvanced(0, 0, "serial.baudrate"),
        String::new(),
    );
    type_into(
        &mut tui.state,
        FieldKey::DutAdvanced(0, 0, "serial.baudrate"),
        "921600",
    );
    let answers = tui
        .apply_action(FormAction::Submit)
        .unwrap()
        .expect("clean F2 with the gate OFF");
    assert_eq!(
        answers.hosts[0].duts[0]
            .advanced
            .get("monitor.crash_keywords")
            .map(String::as_str),
        Some("Oops:")
    );
    assert!(
        answers.advanced.is_empty(),
        "host-level set untouched while the gate is OFF"
    );
    // The write path lands the values in the DUT's override sections.
    let (prepared, opts) = tui.write_ctx.take().unwrap();
    init::finish_init(&mut tui, &opts, &prepared, &answers).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let parsed = sermcp::config::parse_jsonc_text(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let dut = &parsed.dev_hosts[0].duts[0];
    assert_eq!(
        dut.serial
            .as_ref()
            .unwrap()
            .baudrate
            .as_ref()
            .unwrap()
            .to_value_string(),
        "921600"
    );
    assert_eq!(
        dut.monitor
            .as_ref()
            .unwrap()
            .crash_keywords
            .as_ref()
            .unwrap(),
        &["Oops:".to_string()]
    );
    assert!(
        parsed.serial.is_none() && parsed.monitor.is_none(),
        "global sections untouched: {text}"
    );
}

/// dev_ctrl round-trip (field-observed): pick ch340-relay in the TUI
/// controller picker, Save & Exit, re-run `dutabo init` — the field
/// must come back prefilled, never cleared.
#[test]
fn test_dev_ctrl_save_then_reinit_prefills() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    // The REAL repo-shaped config — comments and all (the UPDATE
    // text-patch must locate members around the comment lines).
    const FIXTURE: &str = r#"// .target.jsonc — Embedded Debug Target Configuration (JSONC)
// Copy to project root. // comments and trailing commas are valid.
// Create/update via `dutabo init` — it preserves your comments.
//
// Structure: dev_hosts[] each NESTS its duts[]. A DUT inherits its parent
// host's ip/user/pass — names are display-only; the host ip is the linking key.
// Top-level sections (serial/target/uboot/relay/monitor/flash/control) are
// GLOBAL fallbacks: a DUT value overrides; otherwise the global value wins.
//   commented out = not set (built-in default)
//   port 0 / "0"  = unset sentinel (validation error, as before)
//   ""            = explicitly empty -> falls back to parent dev host ip
//                    (relay.ip only — serial has no ip key at all)
// Unknown keys are rejected at parse time (typos fail loudly).
// The agent must NOT modify this file — read only. Edit via `dutabo init`.
{
  "dev_hosts": [
    {
      "ip": "host-a.invalid",           // REQUIRED — ssh / ser2net host
      "user": "linaro",                // ssh user (shown as user@ip in dutabo list)
      "pass": "linaro",                // ssh password (optional, plaintext as before)
      // "host_name": ""                   // optional display name — falls back to the host ip when absent
      "host_name": "rk3576",           // optional display name — falls back to the host ip when absent
      "duts": [
        {
          "dut_name": "rk3576-ubuntu", // REQUIRED — unique across ALL DUTs
          "serial": {
            "port": 41002,        // ser2net port OR device path "/dev/ttyUSB0"; number or string
            "baudrate": 460800,  // number or string (default 460800)
            // "rfc2217": false            // true for telnet(rfc2217) ser2net ports (dynamic baud)
          },
          "target": {
            "login_user": "root", // empty = boot-to-shell detection disabled (warn)
            // "login_prompt": ""          // regex; empty = built-in "login:" matcher
          },
          "uboot": {
            "interrupt_char": "ctrl_c", // any 1 ASCII char is literal ("2" sends '2'); use "0x02" for a raw control byte
          },
          "monitor": {
            "reference_log": ".dut-serial/rk3576-ubuntu/reference-boot.log",
          },
        },
      ]
    },
  ],
  // Global fallbacks (optional) — apply to every DUT that does not override:
  // "serial":  { "port": 41000, "baudrate": 1500000, "rfc2217": false },
  // "target":  { "login_user": "root", "login_prompt": "" },
  // "uboot":   { "interrupt_char": "ctrl_c", "interrupt_strategy": "flood" },
  // "relay":   { "type": "ch340", "reset_ch": 1, "power_off_time_ms": 3000 },
  // "monitor": { "hang_timeout": 60, "max_archived_logs": 10, "crash_keywords": ["Kernel panic"] },
  // "flash":   { "tool": "upgrade_tool", "upload_dir": "/tmp" },
  // "control": { "dev_ctrl": "" },
  // "reference_log": "",       // back-compat top-level key (overrides monitor.reference_log)
  // "dut_dir": ".dut-serial",  // back-compat top-level key
  // "lock_dir": "/tmp/sermcp/locks"   // top-level ONLY — never valid inside a dut
}
"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    // ── run 1: open the picker on dev_ctrl, confirm, save & exit ──
    let opts = InitOptions::default();
    let prepared = prepare_init(tmp.path(), &opts).unwrap();
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        FormState::new(
            prepared.schema.clone(),
            prepared.target_path.display().to_string(),
        ),
    );
    tui.write_ctx = Some((prepared.clone(), opts.clone()));
    let ctrl_key = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    tui.state.set_focus(FocusSlot::Field(ctrl_key.clone()));
    handle_form_key(&mut tui.state, chr(' ')); // Space opens the picker
    assert!(tui.state.picker.is_some(), "picker must open");
    handle_form_key(&mut tui.state, enter()); // Enter picks options[0]
    assert_eq!(
        tui.state.values.get(&ctrl_key).map(String::as_str),
        Some("ch340-relay"),
        "picker choice must land in the buffer"
    );
    let answers = tui
        .apply_action(FormAction::Submit)
        .unwrap()
        .expect("clean submit");
    assert_eq!(
        answers.hosts[0].duts[0]
            .advanced
            .get("control.dev_ctrl")
            .map(String::as_str),
        Some("ch340-relay"),
        "the answer must carry dev_ctrl"
    );
    init::finish_init(&mut tui, &opts, &prepared, &answers).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let parsed = sermcp::config::parse_jsonc_text(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(
        parsed.dev_hosts[0].duts[0]
            .control
            .as_ref()
            .and_then(|c| c.dev_ctrl.as_deref()),
        Some("ch340-relay"),
        "the file must carry control.dev_ctrl after save: {text}"
    );
    // ── run 2: re-init — the field must come back prefilled ──
    let prepared2 = prepare_init(tmp.path(), &opts).unwrap();
    let state2 = FormState::new(
        prepared2.schema,
        prepared2.target_path.display().to_string(),
    );
    assert_eq!(
        state2.values.get(&ctrl_key).map(String::as_str),
        Some("ch340-relay"),
        "dev_ctrl must prefill on re-run — it was cleared"
    );
}

/// Regression (field-observed): drive the EXACT real key sequence
/// (19 Tabs → Space → Enter → F2) through the full form_loop — the
/// saved answers MUST carry dev_ctrl (it was "unchanged" in the report).
#[test]
fn test_dev_ctrl_real_key_sequence_answers() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "root",
      "pass": "",
      "duts": [
        { "dut_name": "dut1", "serial": { "port": 41002 } }
      ]
    }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let opts = InitOptions::default();
    let prepared = prepare_init(tmp.path(), &opts).unwrap();
    let mut events = Vec::new();
    for _ in 0..19 {
        events.push(ev(tab()));
    }
    events.push(ev(chr(' '))); // open the picker
    events.push(ev(enter())); // pick options[0]
    events.push(ev(f2())); // Save & Exit
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(events),
        FormState::new(
            prepared.schema.clone(),
            prepared.target_path.display().to_string(),
        ),
    );
    tui.write_ctx = Some((prepared.clone(), opts.clone()));
    let answers = tui.form_loop().unwrap();
    let dut = &answers.hosts[0].duts[0];
    assert_eq!(
        dut.advanced.get("control.dev_ctrl").map(String::as_str),
        Some("ch340-relay"),
        "the exact user key sequence must save dev_ctrl"
    );
    assert!(dut.relay_gate, "dev_ctrl opens the relay gate");
}

/// CREATE → re-run variant of the same field-observed: a fresh project
/// saves dev_ctrl during the first `dutabo init`; the second run must
/// prefill it back.
#[test]
fn test_dev_ctrl_create_then_reinit_prefills() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let path = tmp.path().join(".target.jsonc");
    assert!(!path.exists(), "CREATE: no existing config");
    let opts = InitOptions::default();
    let prepared = prepare_init(tmp.path(), &opts).unwrap();
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        FormState::new(
            prepared.schema.clone(),
            prepared.target_path.display().to_string(),
        ),
    );
    tui.write_ctx = Some((prepared.clone(), opts.clone()));
    // Required fields first, then the dev_ctrl picker choice.
    type_into(&mut tui.state, FieldKey::HostIp(0), "10.0.0.1");
    type_into(&mut tui.state, FieldKey::DutPort(0, 0), "41002");
    let ctrl_key = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    tui.state.set_focus(FocusSlot::Field(ctrl_key.clone()));
    handle_form_key(&mut tui.state, chr(' ')); // open the picker
    handle_form_key(&mut tui.state, enter()); // pick options[0]
    assert_eq!(
        tui.state.values.get(&ctrl_key).map(String::as_str),
        Some("ch340-relay")
    );
    let answers = tui
        .apply_action(FormAction::Submit)
        .unwrap()
        .expect("clean create submit");
    assert_eq!(
        answers.hosts[0].duts[0]
            .advanced
            .get("control.dev_ctrl")
            .map(String::as_str),
        Some("ch340-relay")
    );
    init::finish_init(&mut tui, &opts, &prepared, &answers).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let parsed = sermcp::config::parse_jsonc_text(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(
        parsed.dev_hosts[0].duts[0]
            .control
            .as_ref()
            .and_then(|c| c.dev_ctrl.as_deref()),
        Some("ch340-relay"),
        "CREATE must write control.dev_ctrl: {text}"
    );
    // Re-run on the created file: the field must prefill.
    let prepared2 = prepare_init(tmp.path(), &opts).unwrap();
    let state2 = FormState::new(
        prepared2.schema,
        prepared2.target_path.display().to_string(),
    );
    assert_eq!(
        state2.values.get(&ctrl_key).map(String::as_str),
        Some("ch340-relay"),
        "CREATE→re-run must prefill dev_ctrl"
    );
}

/// User report: typed relay port / reset_ch were
/// silently dropped when dev_ctrl was empty — the relay rows are
/// focusable regardless, so the gate must open from typed relay
/// values too, not only from a chosen dev_ctrl backend.
#[test]
fn test_relay_values_saved_without_dev_ctrl() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "root",
      "pass": "",
      "duts": [
        { "dut_name": "dut1", "serial": { "port": 41002 } }
      ]
    }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let opts = InitOptions::default();
    let prepared = prepare_init(tmp.path(), &opts).unwrap();
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        FormState::new(
            prepared.schema.clone(),
            prepared.target_path.display().to_string(),
        ),
    );
    tui.write_ctx = Some((prepared.clone(), opts.clone()));
    // Fill ONLY the relay sub-fields — no dev_ctrl choice.
    type_into(&mut tui.state, FieldKey::DutRelayPort(0, 0), "4001");
    type_into(&mut tui.state, FieldKey::DutRelayReset(0, 0), "1");
    let answers = tui
        .apply_action(FormAction::Submit)
        .unwrap()
        .expect("clean submit");
    let dut = &answers.hosts[0].duts[0];
    assert!(dut.relay_gate, "typed relay values must open the gate");
    assert_eq!(dut.relay_port, "4001");
    assert_eq!(dut.relay_reset_ch, "1");
    let (prepared, opts) = tui.write_ctx.take().unwrap();
    init::finish_init(&mut tui, &opts, &prepared, &answers).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let parsed = sermcp::config::parse_jsonc_text(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let dut = &parsed.dev_hosts[0].duts[0];
    let relay = dut.relay.as_ref().expect("relay section must be written");
    assert_eq!(relay.port, Some(4001), "relay.port must persist: {text}");
    assert_eq!(
        relay.reset_ch,
        Some(1),
        "relay.reset_ch must persist: {text}"
    );
}

/// UPDATE prefill: each DUT's OWN override lands in its fields; a
/// global-only value never prefills a per-DUT buffer (an untouched
/// F2 must not copy fallbacks into DUT sections).
#[test]
fn test_update_prefill_lands_dut_advanced_in_right_dut() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "root",
      "pass": "",
      "duts": [
        {
          "dut_name": "dut1",
          "serial": { "port": 41002, "baudrate": 460800 },
          "monitor": { "crash_keywords": ["A"] }
        },
        {
          "dut_name": "dut2",
          "serial": { "port": 41003 },
          "monitor": { "crash_keywords": ["B"] }
        }
      ]
    }
  ],
  "serial": { "baudrate": 460800 }
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let state = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords"))
            .map(String::as_str),
        Some("A")
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutAdvanced(0, 1, "monitor.crash_keywords"))
            .map(String::as_str),
        Some("B")
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutAdvanced(0, 0, "serial.baudrate"))
            .map(String::as_str),
        Some("460800"),
        "an explicit per-DUT baudrate must prefill its own buffer"
    );
    // The global-only baudrate must not be copied into dut2's buffer.
    assert!(
        !state
            .values
            .contains_key(&FieldKey::DutAdvanced(0, 1, "serial.baudrate")),
        "global fallback values never prefill another DUT's buffer: {:?}",
        state.values
    );
    // The dim hint DOES show the effective fallback (override > global).
    assert_eq!(
        state
            .fields
            .iter()
            .find(|f| f.key == FieldKey::DutAdvanced(0, 0, "serial.baudrate"))
            .and_then(|f| f.default.clone())
            .as_deref(),
        Some("460800")
    );
}

#[test]
fn test_add_host_button_adds_row_with_default_dut() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Button(ButtonId::AddHost);
    handle_form_key(&mut state, chr(' '));
    assert_eq!(state.host_count, 2);
    assert!(state.fields.iter().any(|f| f.key == FieldKey::HostIp(1)));
    // The new host SEEDS one default DUT.
    assert_eq!(state.dut_counts[1], 1);
    assert!(
        state
            .fields
            .iter()
            .any(|f| f.key == FieldKey::DutPort(1, 0))
    );
    assert!(!state.values.contains_key(&FieldKey::HostIp(1)));
    // Direct mutation is the same code path the mouse uses.
    state.add_host();
    assert_eq!(state.host_count, 3);
}

#[test]
fn test_add_dut_button_adds_to_specific_host() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Button(ButtonId::AddDut(0));
    handle_form_key(&mut state, enter());
    assert_eq!(state.dut_counts[0], 2);
    assert!(
        state
            .fields
            .iter()
            .any(|f| f.key == FieldKey::DutPort(0, 1))
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayGate(0, 1))
            .map(String::as_str),
        Some("no")
    );
}

#[test]
fn test_remove_host_button_drops_its_duts_and_renumbers() {
    let mut state = create_state(&quick());
    state.add_host();
    assert_eq!(state.host_count, 2);
    type_into(&mut state, FieldKey::HostIp(0), "10.0.0.1");
    type_into(&mut state, FieldKey::HostIp(1), "10.0.0.2");
    state.focus = FocusSlot::Button(ButtonId::RemoveHost(0));
    handle_form_key(&mut state, chr(' '));
    assert_eq!(state.host_count, 1);
    assert_eq!(state.dut_counts, vec![1], "host 1 keeps its seeded DUT");
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("10.0.0.2"),
        "surviving host's values follow the tab renumber"
    );
}

#[test]
fn test_remove_dut_button_drops_one_dut_and_renumbers() {
    let mut state = create_state(&quick());
    state.add_dut(0);
    assert_eq!(state.dut_counts[0], 2);
    type_into(&mut state, FieldKey::DutPort(0, 0), "41001");
    type_into(&mut state, FieldKey::DutPort(0, 1), "41002");
    state.focus = FocusSlot::Button(ButtonId::RemoveDut(0, 0));
    handle_form_key(&mut state, chr(' '));
    assert_eq!(state.dut_counts[0], 1);
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutPort(0, 0))
            .map(String::as_str),
        Some("41002"),
        "the second DUT shifted into slot 0 with its value"
    );
}

#[test]
fn test_add_host_clamps_to_max_99() {
    let mut state = create_state(&quick());
    for _ in 0..(state.schema.host_count_max + 5) {
        state.add_host();
    }
    assert_eq!(state.host_count, state.schema.host_count_max);
    assert!(
        state.note.is_some(),
        "the clamp announces itself in the header note"
    );
}

#[test]
fn test_values_survive_rebuild_by_field_key() {
    let mut state = create_state(&quick());
    type_into(&mut state, FieldKey::HostIp(0), "10.0.0.1");
    state.add_host();
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("10.0.0.1"),
        "typed value survives the rebuild"
    );
    assert!(!state.values.contains_key(&FieldKey::HostIp(1)));
    type_into(&mut state, FieldKey::HostIp(1), "10.0.0.2");
    state.remove_host(1);
    assert_eq!(state.host_count, 1);
    assert!(!state.values.contains_key(&FieldKey::HostIp(1)));
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("10.0.0.1")
    );
}

#[test]
fn test_empty_count_buffer_submits_committed_visible_structure() {
    let mut tui = test_tui();
    tui.state.add_host();
    type_into(&mut tui.state, FieldKey::HostIp(1), "10.0.0.2");
    assert_eq!(tui.state.host_count, 2, "visible structure kept");
    tui.state.add_dut(0);
    tui.state.add_dut(0);
    type_into(&mut tui.state, FieldKey::DutPort(0, 2), "41004");
    // Host 2 keeps its seeded dut1 while host 1 grew to 3.
    assert_eq!(tui.state.dut_counts, vec![3, 1], "visible structure kept");
    tui.state.add_dut(1);
    type_into(&mut tui.state, FieldKey::HostIp(0), "10.0.0.1");
    type_into(&mut tui.state, FieldKey::DutPort(0, 0), "41002");
    type_into(&mut tui.state, FieldKey::DutPort(0, 1), "41003");
    type_into(&mut tui.state, FieldKey::DutPort(1, 0), "41005");
    type_into(&mut tui.state, FieldKey::DutPort(1, 1), "41006");
    let answers = tui
        .apply_action(FormAction::Submit)
        .unwrap()
        .expect("clean submit");
    assert_eq!(answers.hosts.len(), 2);
    assert_eq!(answers.hosts[1].ip, "10.0.0.2");
    assert_eq!(answers.hosts[0].duts.len(), 3);
    assert_eq!(answers.hosts[0].duts[2].serial_port, "41004");
    assert_eq!(answers.hosts[1].duts.len(), 2);
    assert_eq!(answers.hosts[1].duts[0].serial_port, "41005");
    assert_eq!(answers.hosts[1].duts[1].serial_port, "41006");
}

#[test]
fn test_update_button_added_structure_survives_submit() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    { "ip": "10.0.0.1", "user": "", "pass": "", "duts": [ { "dut_name": "dut1", "serial": { "port": 41002 } } ] }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let mut state = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    assert_eq!(
        state.values.get(&FieldKey::HostCount).map(String::as_str),
        Some("1")
    );
    assert_eq!(
        state.values.get(&FieldKey::DutCount(0)).map(String::as_str),
        Some("1")
    );
    state.add_dut(0);
    state.add_host();
    type_into(&mut state, FieldKey::DutPort(0, 1), "41003");
    type_into(&mut state, FieldKey::DutPort(1, 0), "41004");
    type_into(&mut state, FieldKey::HostIp(1), "10.0.0.2");
    let submitted = state.submit_values();
    assert_eq!(
        submitted.get(&FieldKey::HostCount).map(String::as_str),
        Some("2"),
        "the committed host count wins over the stale prefill"
    );
    assert_eq!(
        submitted.get(&FieldKey::DutCount(0)).map(String::as_str),
        Some("2"),
        "the committed DUT count wins over the stale prefill"
    );
    assert_eq!(
        submitted.get(&FieldKey::DutCount(1)).map(String::as_str),
        Some("1"),
        "the added host commits its seeded DUT"
    );
    let answers = resolve_answers(
        &state.schema,
        &submitted,
        state.gate,
        NamePolicy::ErrorOnCollision,
    )
    .unwrap();
    assert_eq!(answers.hosts.len(), 2, "the added host survives submit");
    assert_eq!(answers.hosts[0].duts.len(), 2);
    assert_eq!(answers.hosts[0].duts[1].serial_port, "41003");
    assert_eq!(answers.hosts[1].ip, "10.0.0.2");
}

#[test]
fn test_shrink_below_existing_shows_amber_note() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    { "ip": "10.0.0.1", "user": "", "pass": "", "duts": [ { "dut_name": "dut1", "serial": { "port": 41002 } } ] },
    { "ip": "10.0.0.2", "user": "", "pass": "", "duts": [ { "dut_name": "dut2", "serial": { "port": 41003 } } ] }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let mut state = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    assert_eq!(state.host_count, 2);
    state.remove_host(1);
    assert_eq!(state.host_count, 1);
    assert!(
        state
            .note
            .as_ref()
            .is_some_and(|(_, m)| m.contains("reduced below existing 2")),
        "{:?}",
        state.note
    );
    assert!(!state.values.contains_key(&FieldKey::HostIp(1)));
}

/// The amber append-only note RENDERS as a yellow transient line
/// inside the affected panel (host panel for a host-count shrink, DUT
/// content box for a DUT-count shrink).
#[test]
fn test_amber_note_renders_inside_affected_panel() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1", "user": "", "pass": "",
      "duts": [
        { "dut_name": "dut1", "serial": { "port": 41002 } },
        { "dut_name": "dut2", "serial": { "port": 41003 } }
      ]
    },
    { "ip": "10.0.0.2", "user": "", "pass": "", "duts": [] }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    // Host-count shrink: the note is the host panel's LAST laid-out
    // row (80x40 so the host keeps its natural height — a 24-row
    // terminal squeezes the host and the note row scrolls out).
    let mut state = FormState::new(
        prepared.schema.clone(),
        prepared.target_path.display().to_string(),
    );
    state.remove_host(1);
    let terminal = terminal_with(&mut state, 80, 40);
    let buf = terminal.backend().buffer();
    let map = form_layout(Rect::new(0, 0, 80, 40), &state);
    let note_row = &buffer_rows(&terminal)[map.host_panel.bottom().saturating_sub(1) as usize];
    assert!(
        note_row.contains("host count reduced below existing 2"),
        "host note must render as the host panel's last row: {note_row}"
    );
    let x = map.host_panel_inner.x.saturating_add(2);
    let y = map.host_panel.bottom().saturating_sub(1);
    assert_eq!(
        buf.content[buf.index_of(x, y)].fg,
        Color::Yellow,
        "the note renders amber"
    );
    // DUT-count shrink: the note paints at the DUT content box bottom.
    let mut state = FormState::new(
        prepared.schema.clone(),
        prepared.target_path.display().to_string(),
    );
    state.remove_dut(0, 1);
    let terminal = terminal_with(&mut state, 80, 32);
    let buf = terminal.backend().buffer();
    let map = form_layout(Rect::new(0, 0, 80, 32), &state);
    let note_row =
        &buffer_rows(&terminal)[map.dut_content_inner.bottom().saturating_sub(1) as usize];
    assert!(
        note_row.contains("DUT count reduced below existing 2"),
        "DUT note must render inside the content box: {note_row}"
    );
    let x = map.dut_content_inner.x.saturating_add(2);
    let y = map.dut_content_inner.bottom().saturating_sub(1);
    assert_eq!(buf.content[buf.index_of(x, y)].fg, Color::Yellow);
}

/// An error too long to fit beside the value renders on the sub-line
/// row BELOW the value in red, and the field's label turns red.
#[test]
fn test_error_subline_renders_below_value_when_too_long() {
    // DUT content box: the serial.port row's error does not fit
    // beside the 30-cell value → paints on the next row. 120x40 (was
    // 80x24): the richer DUT content (DUT-owned advanced groups) no
    // longer fits an 80x24 box — at that height the box renders
    // borders only and the sub-line row would fall off-screen.
    let mut state = create_state(&quick());
    state.errors.insert(
        FieldKey::DutPort(0, 0),
        "port must be a number between 1 and 65535".into(),
    );
    state.values.insert(FieldKey::DutPort(0, 0), "70000".into());
    let terminal = terminal_with(&mut state, 120, 40);
    let buf = terminal.backend().buffer();
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let (_, vrect) = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::DutPort(0, 0))
        .unwrap();
    let row = map
        .dut_rows
        .iter()
        .find(|r| r.key.as_ref() == Some(&FieldKey::DutPort(0, 0)))
        .unwrap();
    let sub_row = &buffer_rows(&terminal)[(vrect.y + 1) as usize];
    assert!(
        sub_row.contains("port must"),
        "the error text paints on the sub-line row (the 3-column box's \
             narrower value cell clips the tail): {sub_row}"
    );
    assert_eq!(
        buf.content[buf.index_of(vrect.x, vrect.y + 1)].fg,
        Color::Red,
        "the sub-line error is red"
    );
    assert_eq!(
        buf.content[buf.index_of(row.label_rect.x, row.label_rect.y)].fg,
        Color::Red,
        "the erroring label turns red"
    );
}

#[test]
fn test_editor_state_survives_rebuild() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    for c in "10.0.0.1".chars() {
        handle_form_key(&mut state, chr(c));
    }
    handle_form_key(&mut state, ctrl('b')); // cursor 7 (emacs)
    let before = state.editors.get(&FieldKey::HostIp(0)).copied().unwrap();
    state.add_host();
    assert_eq!(
        state.editors.get(&FieldKey::HostIp(0)).copied().unwrap(),
        before,
        "per-field editor state survives the rebuild"
    );
}

#[test]
fn test_reference_log_hint_recomposes_live() {
    let mut state = create_state(&InitOptions::default());
    let composed = |s: &FormState| {
        s.fields
            .iter()
            .find(|f| f.key == FieldKey::DutAdvanced(0, 0, "monitor.reference_log"))
            .and_then(|f| f.default.clone())
    };
    assert_eq!(
        composed(&state).as_deref(),
        Some(".dut-serial/dut1/reference-boot.log")
    );
    state.set_value(FieldKey::DutName(0, 0), "boardX".into());
    state.recompute_name_hints();
    assert_eq!(
        composed(&state).as_deref(),
        Some(".dut-serial/boardX/reference-boot.log")
    );
}

// ── §7.1 #1-10: the NEW state-model tests ──────────────────────────

/// #1: initial focus is the first host tab; Tab/Shift-Tab cycle the
/// ZONES (Host Tabs → Host Fields → DUT Tabs → DUT Fields) wrapping.
#[test]
fn test_focus_zones_cycle_with_tab() {
    let mut state = create_state(&quick());
    assert_eq!(state.focus, FocusSlot::HostTab(0));
    // Quick mode: the zone walk is
    // HostTab(0) → AddHost → HostIp → HostUser → HostPass → HostName
    // → DutTab(0) → AddDut(0) → the DUT BOXES in canonical order
    // (box-major — identity → relay → flash → console →
    // monitor; the login triple sits in identity BEFORE dev_ctrl and
    // the interrupt pair moved to the console box AFTER flash.*):
    // DutName → DutPort → serial.baudrate → DutLoginUser →
    // target.login_pass → target.login_prompt → control.dev_ctrl →
    // DutRelayPort → reset/maskrom/recovery/power ch → reset/power-off
    // time → flash.* → interrupt_* → monitor.* → wrap.
    for expected in [
        FocusSlot::Button(ButtonId::AddHost),
        FocusSlot::Field(FieldKey::HostIp(0)),
        FocusSlot::Field(FieldKey::HostUser(0)),
        FocusSlot::Field(FieldKey::HostPass(0)),
        FocusSlot::Field(FieldKey::HostName(0)),
        FocusSlot::DutTab(0),
        FocusSlot::Button(ButtonId::AddDut(0)),
        FocusSlot::Field(FieldKey::DutName(0, 0)),
        FocusSlot::Field(FieldKey::DutPort(0, 0)),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "serial.baudrate")),
        FocusSlot::Field(FieldKey::DutLoginUser(0, 0)),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "target.login_pass")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "target.login_prompt")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "control.dev_ctrl")),
        FocusSlot::Field(FieldKey::DutRelayPort(0, 0)),
        FocusSlot::Field(FieldKey::DutRelayReset(0, 0)),
        FocusSlot::Field(FieldKey::DutRelayMaskrom(0, 0)),
        FocusSlot::Field(FieldKey::DutRelayRecovery(0, 0)),
        FocusSlot::Field(FieldKey::DutRelayPower(0, 0)),
        FocusSlot::Field(FieldKey::DutRelayResetTimeMs(0, 0)),
        FocusSlot::Field(FieldKey::DutRelayPowerOffTimeMs(0, 0)),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.tool")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.loader_bin")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.loader_cmd")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.list_devices_cmd")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.full_image_cmd")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.kernel_image_cmd")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "flash.upload_dir")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "loader")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "uboot.interrupt_char")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "uboot.interrupt_strategy")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "monitor.reference_log")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "monitor.max_archived_logs")),
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords")),
        FocusSlot::Button(ButtonId::Save),
        FocusSlot::Button(ButtonId::Test),
        FocusSlot::Button(ButtonId::SaveExit),
    ] {
        handle_form_key(&mut state, tab());
        assert_eq!(state.focus, expected);
    }
    // Wraps back to the first zone.
    handle_form_key(&mut state, tab());
    assert_eq!(state.focus, FocusSlot::HostTab(0));
    // Shift-Tab walks backwards (SaveExit is the last slot; the last
    // DUT-zone field is now monitor.crash_keywords).
    handle_form_key(&mut state, key(KeyCode::BackTab));
    assert_eq!(state.focus, FocusSlot::Button(ButtonId::SaveExit));
    handle_form_key(&mut state, key(KeyCode::BackTab));
    assert_eq!(state.focus, FocusSlot::Button(ButtonId::Test));
    handle_form_key(&mut state, key(KeyCode::BackTab));
    assert_eq!(state.focus, FocusSlot::Button(ButtonId::Save));
    handle_form_key(&mut state, key(KeyCode::BackTab));
    assert_eq!(
        state.focus,
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords"))
    );
}

/// #1b: at zero hosts the ONLY focusable slot is AddHost.
#[test]
fn test_zero_hosts_focus_is_add_host_button() {
    let mut state = create_state(&quick());
    state.remove_host(0);
    assert_eq!(state.focus, FocusSlot::Button(ButtonId::AddHost));
    assert_eq!(
        state.focus_order(),
        vec![
            FocusSlot::Button(ButtonId::AddHost),
            FocusSlot::Button(ButtonId::Save),
            FocusSlot::Button(ButtonId::Test),
            FocusSlot::Button(ButtonId::SaveExit),
        ]
    );
}

/// #2: ←/→ switch tabs WITHIN the focused tab group (wrapping).
#[test]
fn test_arrows_switch_within_focused_tab_group() {
    let mut state = create_state(&quick());
    state.add_host();
    assert_eq!(state.selected_host, 1, "add_host auto-selects");
    state.set_focus(FocusSlot::HostTab(1));
    handle_form_key(&mut state, key(KeyCode::Right));
    assert_eq!(state.focus, FocusSlot::HostTab(0));
    handle_form_key(&mut state, key(KeyCode::Right));
    assert_eq!(state.focus, FocusSlot::HostTab(1));
    handle_form_key(&mut state, key(KeyCode::Left));
    assert_eq!(state.focus, FocusSlot::HostTab(0));
    // DUT tabs.
    state.add_dut(0);
    state.add_dut(0);
    assert_eq!(state.selected_dut, Some(2));
    state.set_focus(FocusSlot::DutTab(2));
    handle_form_key(&mut state, key(KeyCode::Right));
    assert_eq!(state.focus, FocusSlot::DutTab(0));
    handle_form_key(&mut state, key(KeyCode::Left));
    assert_eq!(state.focus, FocusSlot::DutTab(2));
}

/// #3: clicking a host tab switches the CURRENT host — BOTH panels
/// update together (only that host's fields in the layout map).
#[test]
fn test_click_host_tab_switches_host_and_panels() {
    let mut state = create_state(&quick());
    state.add_host();
    state.add_dut(1); // host 2 gets its own DUT
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        state,
    );
    let size = tui.terminal.size().unwrap();
    let map = form_layout(Rect::new(0, 0, size.width, size.height), &tui.state);
    let (_, tab1) = map.host_tabs[0];
    tui.handle_mouse(mouse_down(tab1.x + 2, tab1.y)).unwrap();
    assert_eq!(tui.state.selected_host, 0);
    assert_eq!(tui.state.focus, FocusSlot::HostTab(0));
    // Both panels update: host 0's fields only, host 0's DUT tabs only.
    let map2 = form_layout(Rect::new(0, 0, size.width, size.height), &tui.state);
    assert!(map2.fields.iter().any(|(k, _)| *k == FieldKey::HostIp(0)));
    assert!(!map2.fields.iter().any(|(k, _)| *k == FieldKey::HostIp(1)));
    assert!(map2.dut_tabs.iter().any(|(d, _)| *d == 0));
    assert!(!map2.dut_tabs.iter().any(|(d, _)| *d == 1));
    // Clicking a DUT tab shows ONLY that DUT.
    tui.state.add_dut(0);
    let map3 = form_layout(Rect::new(0, 0, size.width, size.height), &tui.state);
    let (_, dut0) = map3.dut_tabs[0];
    tui.handle_mouse(mouse_down(dut0.x + 2, dut0.y)).unwrap();
    assert_eq!(tui.state.selected_dut, Some(0));
    assert_eq!(tui.state.focus, FocusSlot::DutTab(0));
}

/// #4: add-host/add-DUT auto-select the new item; Del (tab focus)
/// removes the CURRENT host/DUT with clamps.
#[test]
fn test_add_host_selects_new_host() {
    let mut state = create_state(&quick());
    state.add_host();
    assert_eq!(state.selected_host, 1);
    assert_eq!(state.selected_dut, Some(0), "the new host seeds one DUT");
    assert_eq!(state.focus, FocusSlot::HostTab(1));
}

#[test]
fn test_add_dut_selects_new_dut() {
    let mut state = create_state(&quick());
    state.add_dut(0);
    assert_eq!(state.selected_host, 0);
    assert_eq!(state.selected_dut, Some(1));
    assert_eq!(state.focus, FocusSlot::DutTab(1));
}

#[test]
fn test_del_removes_current_host_or_dut_with_clamps() {
    let mut state = create_state(&quick());
    state.add_host();
    // Del with the host tab focused removes the current host.
    state.set_focus(FocusSlot::HostTab(1));
    handle_form_key(&mut state, del());
    assert_eq!(state.host_count, 1);
    assert_eq!(state.selected_host, 0, "clamped after removal");
    // Del with a DUT tab focused removes the current DUT (clamped).
    state.add_dut(0);
    state.add_dut(0);
    assert_eq!(state.selected_dut, Some(2));
    state.set_focus(FocusSlot::DutTab(2));
    handle_form_key(&mut state, del());
    assert_eq!(state.dut_counts[0], 2);
    assert_eq!(state.selected_dut, Some(1), "clamped to the shifted-in DUT");
    // Removing the last host → the AddHost button holds focus.
    state.remove_host(0);
    assert_eq!(state.host_count, 0);
    assert_eq!(state.focus, FocusSlot::Button(ButtonId::AddHost));
}

/// #5: F2 saves DIRECTLY — answers resolved once, `.target.jsonc`
/// written (tmp+rename), `.mcp.json` reconciled, NO Review screen.
#[test]
fn test_f2_saves_directly_no_review() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let mut events: Vec<Event> = Vec::new();
    // Focus starts on HostTab(0); Tab walks: AddHost → HostIp; then →
    // HostUser → HostPass → HostName → DutTab → AddDut → DutName →
    // DutPort (seven tabs from HostIp — the DUT zone now
    // continues serial.baudrate → login user → login pass →
    // login_prompt → dev_ctrl → … after the port, so the typed port
    // must land on tab #8 exactly).
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.extend(typed("10.0.0.1"));
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.extend(typed("41002"));
    events.push(ev(f2())); // F2 saves directly
    let outcome = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap();
    assert!(outcome.config_created);
    let text = std::fs::read_to_string(tmp.path().join(".target.jsonc")).unwrap();
    assert!(text.contains("\"dut_name\": \"dut1\""), "{text}");
    assert!(text.contains("41002"), "{text}");
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
    assert!(
        outcome.changes.iter().any(|c| c.contains(".mcp.json")),
        ".mcp.json reconciled: {:?}",
        outcome.changes
    );
}

// ── Occupancy gate ────────────────────────────────────

/// RAII live endpoint lock (our own PID, always alive) in the shared
/// default lock dir — removed on drop so a panicking test can never
/// leak a lock that breaks later tests in the single-threaded suite.
struct EndpointHold {
    path: std::path::PathBuf,
}

impl Drop for EndpointHold {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn hold_endpoint(host: &str, port: &str) -> EndpointHold {
    let lock_dir = sermcp::lock_manager::default_lock_dir();
    std::fs::create_dir_all(&lock_dir).unwrap();
    let path = sermcp::lock_manager::endpoint_lock_path(host, port, &lock_dir);
    let since = chrono::Utc::now().to_rfc3339();
    let content = format!("{}\n{host}:{port}\nhost\n{since}", std::process::id());
    std::fs::write(&path, content).unwrap();
    EndpointHold { path }
}

/// The F2 save sequence for a quick CREATE with a given serial port
/// (focus: HostTab(0) → Tab → AddHost → HostIp → Tab×7 → DutPort).
fn fill_save_events(port: &str) -> Vec<Event> {
    let mut events: Vec<Event> = Vec::new();
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.extend(typed("10.0.0.1"));
    for _ in 0..7 {
        events.push(ev(tab()));
    }
    events.extend(typed(port));
    events.push(ev(f2())); // F2 saves directly
    events
}

/// F2 with a serial endpoint held by another live MCP opens the
/// occupancy-gate modal; Esc aborts and writes NOTHING. The holder's
/// lock file stays untouched — its session is never disturbed.
#[test]
fn test_busy_gate_esc_aborts_and_writes_nothing() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let port = "47121";
    let hold = hold_endpoint("10.0.0.1", port);
    let mut events = fill_save_events(port);
    events.push(ev(esc())); // modal: abort
    let err = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap_err();
    assert!(err.contains("aborted"), "{err}");
    assert!(err.contains("10.0.0.1"), "{err}");
    assert!(err.contains(port), "{err}");
    assert!(
        !tmp.path().join(".target.jsonc").exists(),
        "abort writes zero bytes"
    );
    assert!(hold.path.exists(), "the holder's lock is NEVER touched");
}

/// Arrowing to [continue writing] then Enter writes the config and
/// still leaves the holder's lock untouched. The modal's default cursor
/// is ABORT — the safe choice.
#[test]
fn test_busy_gate_arrow_enter_continues_and_writes() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let port = "47122";
    let hold = hold_endpoint("10.0.0.1", port);
    let mut events = fill_save_events(port);
    events.push(ev(down())); // cursor: abort → continue writing
    events.push(ev(enter())); // confirm continue
    let outcome = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap();
    assert!(outcome.config_created);
    let text = std::fs::read_to_string(tmp.path().join(".target.jsonc")).unwrap();
    assert!(text.contains(port), "{text}");
    assert!(hold.path.exists(), "the holder's lock is NEVER touched");
}

/// The modal renders every conflict detail (endpoint, PID, project,
/// since) plus the two choices.
#[test]
fn test_busy_gate_renders_conflict_details() {
    let mut tui = test_tui();
    tui.state.busy_gate = Some(BusyGate {
        conflicts: vec![EndpointConflict {
            host: "10.0.0.1".into(),
            port: "47123".into(),
            pid: 12345,
            since: chrono::Utc::now().to_rfc3339(),
            owner_project: Some("/media/other/proj".into()),
        }],
        cursor: 1,
    });
    tui.draw().unwrap();
    let joined = buffer_rows(&tui.terminal).join("\n");
    assert!(
        joined.contains("serial endpoint held by another MCP"),
        "{joined}"
    );
    assert!(joined.contains("10.0.0.1:47123"), "{joined}");
    assert!(joined.contains("PID 12345"), "{joined}");
    assert!(joined.contains("/media/other/proj"), "{joined}");
    assert!(joined.contains("continue writing"), "{joined}");
    assert!(joined.contains("abort"), "{joined}");
    tui.state.busy_gate = None;
}

/// #6: F2 with errors writes NOTHING and focuses the first error.
#[test]
fn test_f2_with_errors_writes_nothing_and_focuses_first_error() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    // Empty CREATE: ip + port are required with empty defaults.
    let mut tui = test_tui();
    let res = tui.apply_action(FormAction::Submit).unwrap();
    assert!(res.is_none());
    assert_eq!(tui.state.errors.len(), 2, "{:?}", tui.state.errors);
    assert_eq!(tui.state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
    // End-to-end: F2 with nothing typed → nothing written. A trailing
    // Esc proves the loop KEPT RUNNING after the failed save (a
    // hypothetical early-exit submit would return Ok before the Esc
    // and trip the unwrap_err) and aborts cleanly.
    let err = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(vec![ev(f2()), ev(esc())]),
        &quick(),
        tmp.path(),
    )
    .unwrap_err();
    assert_eq!(
        err, "aborted by user",
        "the failed save keeps the loop alive until the user quits"
    );
    assert!(!tmp.path().join(".target.jsonc").exists());
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
}

/// F2 with an ADVANCED field error while `[Other ▲]` is collapsed
/// (Forced mode only — the gate stays on when collapsed) must EXPAND
/// the advanced section before focusing the error: focus must never
/// land on a field the layout does not contain (no rect → no visible
/// highlight, no inline error).
#[test]
fn test_f2_forced_collapsed_expands_for_advanced_error() {
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&InitOptions {
            advanced: true,
            ..Default::default()
        }),
    );
    assert!(tui.state.other_expanded, "Forced starts expanded");
    // Collapse [Other ▲] — Forced keeps the gate on.
    tui.state.focus = FocusSlot::Button(ButtonId::ToggleOther);
    handle_form_key(&mut tui.state, enter());
    assert!(!tui.state.other_expanded);
    assert!(tui.state.gate, "the Forced gate cannot be dismissed");
    // A valid quick form + one INVALID advanced value → the only
    // error is the advanced key.
    type_into(&mut tui.state, FieldKey::HostIp(0), "10.0.0.1");
    type_into(&mut tui.state, FieldKey::DutPort(0, 0), "41002");
    type_into(
        &mut tui.state,
        FieldKey::Advanced("monitor.hang_timeout"),
        "abc",
    );
    let res = tui.apply_action(FormAction::Submit).unwrap();
    assert!(res.is_none(), "the invalid advanced value blocks the save");
    assert!(
        tui.state
            .errors
            .contains_key(&FieldKey::Advanced("monitor.hang_timeout")),
        "{:?}",
        tui.state.errors
    );
    assert_eq!(
        tui.state.focus,
        FocusSlot::Field(FieldKey::Advanced("monitor.hang_timeout"))
    );
    assert!(
        tui.state.other_expanded,
        "the advanced section expands so the error and its focus are visible"
    );
    // The focused field has a real layout rect (a visible target).
    let map = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::Advanced("monitor.hang_timeout")),
        "the erroring advanced field is laid out inside the host panel"
    );
}

/// #7: Enter never submits; Esc/q quit immediately writing nothing.
#[test]
fn test_enter_does_not_submit() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    assert_eq!(handle_form_key(&mut state, enter()), FormAction::Continue);
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
    // On a tab, Enter just selects it — still no submit.
    state.set_focus(FocusSlot::HostTab(0));
    assert_eq!(handle_form_key(&mut state, enter()), FormAction::Continue);
}

/// #7b: Esc and q are immediate Abort actions at the key layer (the
/// pending-quit confirm dialog is deleted). The zero-byte end-to-end
/// guarantee is pinned by `test_e2e_esc_abort_writes_zero_bytes`
/// (§7.1 #22) — the run_form_with tail this test used to carry moved
/// there so the E2E flows live in one place (§8.6).
#[test]
fn test_esc_and_q_quit_immediately_write_nothing() {
    let mut state = create_state(&quick());
    assert_eq!(
        handle_form_key(&mut state, esc()),
        FormAction::Abort("aborted by user".into())
    );
    assert_eq!(
        handle_form_key(&mut state, chr('q')),
        FormAction::Abort("aborted by user".into())
    );
}

/// 'q' is context-sensitive: on tabs/buttons (or an EMPTY text field)
/// it quits, but mid-edit a non-empty text buffer OWNS the letter —
/// "qemu", "q" in passwords etc. must be typeable.
#[test]
fn test_q_types_into_nonempty_text_field_and_quits_elsewhere() {
    let mut state = create_state(&quick());
    // Mid-edit: q is data.
    type_into(&mut state, FieldKey::HostIp(0), "10.0.0.");
    assert_eq!(handle_form_key(&mut state, chr('q')), FormAction::Continue);
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("10.0.0.q"),
        "a stray 'q' press must not destroy the session's edits"
    );
    // A fresh EMPTY field: q quits.
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostUser(0));
    assert_eq!(
        handle_form_key(&mut state, chr('q')),
        FormAction::Abort("aborted by user".into())
    );
    // On a tab it quits.
    let mut state = create_state(&quick());
    state.set_focus(FocusSlot::HostTab(0));
    assert_eq!(
        handle_form_key(&mut state, chr('q')),
        FormAction::Abort("aborted by user".into())
    );
    // On a button it quits.
    let mut state = create_state(&quick());
    state.set_focus(FocusSlot::Button(ButtonId::AddHost));
    assert_eq!(
        handle_form_key(&mut state, chr('q')),
        FormAction::Abort("aborted by user".into())
    );
    // On a Select field with an empty buffer (no text editor yet) it
    // quits too (the gate row is gone — the dev_ctrl
    // Select row is the canonical non-text DUT field now).
    let mut state = create_state(&quick());
    state.set_focus(FocusSlot::Field(FieldKey::DutAdvanced(
        0,
        0,
        "control.dev_ctrl",
    )));
    assert_eq!(
        handle_form_key(&mut state, chr('q')),
        FormAction::Abort("aborted by user".into())
    );
}

/// #8: `[Other ▼]` expands the advanced HOST-LEVEL fields INSIDE the
/// host panel. The advanced section is EXPANDED by DEFAULT ;
/// `[Other ▲]` collapses it; --advanced Forced starts expanded.
/// the DUT-owned advanced keys (target.login_pass /
/// target.login_prompt / uboot.interrupt_char / monitor.reference_log /
/// control.dev_ctrl / flash.*) moved INTO the DUT content box, so they
/// stay in the layout even while [Other] is collapsed and NEVER appear
/// in the host panel's rows.
#[test]
fn test_other_toggle_expands_advanced_inside_host_panel() {
    let mut state = create_state(&InitOptions::default());
    assert!(state.other_expanded, "advanced expanded by default");
    assert!(!state.gate);
    // Collapse first: the gate follows.
    state.focus = FocusSlot::Button(ButtonId::ToggleOther);
    handle_form_key(&mut state, enter());
    assert!(!state.other_expanded);
    assert!(!state.gate);
    let map0 = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert!(
        !map0
            .host_rows
            .iter()
            .any(|r| matches!(&r.key, Some(FieldKey::Advanced(_)))),
        "collapsed: NO advanced fields in the host panel's rows"
    );
    // The moved DUT-owned keys live in the DUT content box instead.
    for moved in [
        "control.dev_ctrl",
        "target.login_pass",
        "target.login_prompt",
        "uboot.interrupt_char",
        "flash.tool",
        "monitor.reference_log",
    ] {
        assert!(
            map0.dut_rows
                .iter()
                .any(|r| r.key.as_ref() == Some(&FieldKey::DutAdvanced(0, 0, moved))),
            "collapsed: {moved} still rendered inside the DUT content box"
        );
    }
    // Expand: the gate turns on and the HOST-LEVEL advanced fields land
    // in the panel.
    handle_form_key(&mut state, enter());
    assert!(state.other_expanded);
    assert!(state.gate, "Confirm mode: expanding turns the gate on");
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert!(map.other_btn.is_some());
    assert_eq!(
        map.other_btn.unwrap().right(),
        map.host_panel.right().saturating_sub(1),
        "the [Other ▲] button ends one cell before the border corner"
    );
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::Advanced("monitor.hang_timeout")),
        "host-level advanced fields render inside the host panel"
    );
    // The expanded host rows are EXACTLY the base four + the shrunken
    // host-level advanced set (4 keys two-up = 2 rows → 6 total;
    // the DUT-owned 16 moved into the DUT content box).
    let host_adv: Vec<&str> = map
        .host_rows
        .iter()
        .filter_map(|r| match &r.key {
            Some(FieldKey::Advanced(k)) => Some(*k),
            _ => None,
        })
        .collect();
    for host_level in [
        "monitor.hang_timeout",
        "monitor.max_log_file_size",
        "monitor.learner_stage_threshold",
        "monitor.learner_crash_threshold",
    ] {
        assert!(
            host_adv.contains(&host_level),
            "expanded: {host_level} must stay in the host panel"
        );
    }
    for moved in [
        "serial.baudrate",
        "target.login_prompt",
        "target.login_pass",
        "uboot.interrupt_char",
        "uboot.interrupt_strategy",
        "monitor.reference_log",
        "monitor.max_archived_logs",
        "monitor.crash_keywords",
        "control.dev_ctrl",
        "flash.tool",
        "flash.loader_bin",
        "flash.loader_cmd",
        "flash.list_devices_cmd",
        "flash.full_image_cmd",
        "flash.kernel_image_cmd",
        "flash.upload_dir",
        "loader",
    ] {
        assert!(
            !host_adv.contains(&moved),
            "expanded: {moved} must be ABSENT from the host panel's rows"
        );
    }
    assert_eq!(host_adv.len(), 4, "the shrunken host-level advanced set");
    assert_eq!(
        map.host_content_height, 4,
        "the expanded panel carries the advanced block rows (3 host fields + 4 advanced, two-up)"
    );
    // Forced (--advanced): starts expanded, gate stays on when collapsed.
    let mut forced = create_state(&InitOptions {
        advanced: true,
        ..Default::default()
    });
    assert!(forced.gate);
    assert!(forced.other_expanded);
    forced.focus = FocusSlot::Button(ButtonId::ToggleOther);
    handle_form_key(&mut forced, enter());
    assert!(!forced.other_expanded);
    assert!(forced.gate, "Forced gate cannot be dismissed");
}

/// #9: zero hosts / zero-DUT host states.
#[test]
fn test_zero_hosts_and_zero_dut_host_states() {
    let mut state = create_state(&quick());
    state.remove_host(0);
    assert_eq!(state.host_count, 0);
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert!(map.host_tabs.is_empty(), "host bar shows only '+'");
    assert!(map.buttons.iter().any(|(b, _)| *b == ButtonId::AddHost));
    assert!(map.dut_tabs.is_empty(), "NO DUT tabs");
    assert!(
        !map.fields
            .iter()
            .any(|(k, _)| { matches!(k, FieldKey::DutPort(..) | FieldKey::DutName(..)) }),
        "no DUT fields in the layout"
    );
    assert!(
        map.host_rows
            .iter()
            .any(|r| r.key.is_none() && r.label.contains("No dev hosts")),
        "dim hint in the DEVELOPMENT HOST panel"
    );
    // A host with zero DUTs: only '+' in the DUT bar + dim hint.
    let mut state = create_state(&quick());
    state.remove_dut(0, 0);
    assert_eq!(state.selected_dut, None);
    assert_eq!(state.focus, FocusSlot::Button(ButtonId::AddDut(0)));
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert!(map.dut_tabs.is_empty());
    assert!(map.buttons.iter().any(|(b, _)| *b == ButtonId::AddDut(0)));
    assert!(
        map.dut_rows
            .iter()
            .any(|r| r.key.is_none() && r.label.contains("No DUTs")),
        "dim hint in the content box"
    );
    assert!(
        !map.fields
            .iter()
            .any(|(k, _)| { matches!(k, FieldKey::DutPort(..)) })
    );
}

/// #10: the submit-values count contract (counts stamped on F2).
#[test]
fn test_submit_values_count_contract() {
    let mut state = create_state(&quick());
    state.add_host();
    state.add_dut(0);
    let submitted = state.submit_values();
    assert_eq!(
        submitted.get(&FieldKey::HostCount).map(String::as_str),
        Some("2")
    );
    assert_eq!(
        submitted.get(&FieldKey::DutCount(0)).map(String::as_str),
        Some("2")
    );
    assert_eq!(
        submitted.get(&FieldKey::DutCount(1)).map(String::as_str),
        Some("1")
    );
}
// ── §7.1 #11-15: the NEW layout tests ──────────────────────────────

/// #11: the four-zone split (header / host / divider / DUT / footer):
/// the host panel hugs its content, the DUT panel hugs its
/// multi-column content (— any leftover space sits BELOW
/// the DUT panel and the footer stays pinned at the bottom), the host
/// grows when [Other ▼] expands, and a pinning drag past the DUT's
/// natural height shrinks it below (its content scrolls).
#[test]
fn test_four_zone_heights() {
    let state = create_state(&quick());
    let area = Rect::new(0, 0, 80, 100);
    let map = form_layout(area, &state);
    let ch = area.height.saturating_sub(1); // header
    let pct = |p: u32| ((ch as u32).saturating_mul(p) / 100).clamp(1, u32::MAX) as u16;
    assert_eq!(map.host_tab_bar.height, pct(7));
    assert_eq!(map.footer.height, pct(7));
    // Content-driven: host height = content + borders,
    // no blank space.
    assert_eq!(
        map.host_panel.height,
        (map.host_content_height as u16).saturating_add(2),
        "host panel hugs its content"
    );
    assert_eq!(map.divider.height, 1u16, "ONE divider line");
    assert_eq!(map.divider.y, map.host_panel.bottom());
    assert_eq!(map.dut_panel.y, map.divider.bottom());
    // Content-hugging: the DUT panel is exactly its
    // multi-column content + chrome (1 tab + 2 box borders + bottom).
    assert_eq!(
        map.dut_panel.height,
        map.dut_content_height as u16 + 4,
        "the DUT panel hugs its content"
    );
    assert_eq!(
        map.footer.y,
        area.bottom().saturating_sub(map.footer.height),
        "the footer stays pinned at the bottom"
    );
    assert!(
        map.dut_panel.bottom() <= map.footer.y,
        "any leftover space sits below the DUT panel"
    );
    // Expanded advanced: the host grows to its content (capped so the
    // DUT keeps its reservation); the DUT panel keeps hugging.
    let expanded = create_state(&InitOptions {
        advanced: true,
        ..Default::default()
    });
    let map2 = form_layout(area, &expanded);
    assert!(map2.host_panel.height > map.host_panel.height, "host grows");
    assert_eq!(
        map2.dut_panel.height, map.dut_panel.height,
        "the DUT panel keeps hugging at 80x100"
    );
    // A pinning drag past the DUT's natural height shrinks it below
    // (the content overflows and scrolls).
    let mut pinned = create_state(&quick());
    pinned.host_h_pin = Some(70);
    let map3 = form_layout(area, &pinned);
    assert_eq!(map3.host_panel.height, 70u16);
    assert_eq!(map3.dut_panel.y, map3.divider.bottom());
    assert!(
        map3.dut_panel.height < map.dut_panel.height,
        "a pinned tall host shrinks the DUT panel below its natural height"
    );
    // The pin overrides (clamped).
    let mut pinned2 = create_state(&quick());
    pinned2.host_h_pin = Some(10);
    let map4 = form_layout(area, &pinned2);
    assert_eq!(map4.host_panel.height, 10u16);
}

/// #12: host/DUT tab geometry + the x/+ button targets.
#[test]
fn test_host_tab_geometry_and_buttons() {
    let mut state = create_state(&quick());
    state.add_host();
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert_eq!(map.host_tabs.len(), 2);
    let (_, t0) = map.host_tabs[0];
    let (_, t1) = map.host_tabs[1];
    assert_eq!(t0.x, map.host_tab_bar.x);
    assert_eq!(t0.y, map.host_tab_bar.y);
    assert_eq!(t0.width, "dev host 1  x ".chars().count() as u16);
    assert_eq!(t1.x, t0.right().saturating_add(1), "one separator cell");
    // The x sub-rect sits one cell inside the tab's right edge (the
    // trailing pad cell separates it from the │ separator).
    let rm = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::RemoveHost(0))
        .unwrap()
        .1;
    assert_eq!(
        rm,
        Rect {
            x: t0.right().saturating_sub(2),
            y: map.host_tab_bar.y,
            width: 1,
            height: 1,
        }
    );
    // The trailing + card at the row end.
    let add = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddHost)
        .unwrap()
        .1;
    assert_eq!(add.x, t1.right().saturating_add(1));
    assert_eq!(add.width, 3);
    assert_eq!(add.y, map.host_tab_bar.y);
}

#[test]
fn test_dut_tab_geometry_and_buttons() {
    let mut state = create_state(&quick());
    state.add_dut(0);
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert_eq!(map.dut_tabs.len(), 2);
    let (_, d0) = map.dut_tabs[0];
    let (_, d1) = map.dut_tabs[1];
    assert_eq!(d0.x, map.dut_tab_bar.x);
    assert_eq!(d0.y, map.dut_tab_bar.y);
    assert_eq!(d0.width, "dut1  x ".chars().count() as u16);
    assert_eq!(d1.x, d0.right().saturating_add(1));
    let rm = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::RemoveDut(0, 0))
        .unwrap()
        .1;
    assert_eq!(
        rm,
        Rect {
            x: d0.right().saturating_sub(2),
            y: map.dut_tab_bar.y,
            width: 1,
            height: 1,
        }
    );
    let add = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddDut(0))
        .unwrap()
        .1;
    assert_eq!(add.x, d1.right().saturating_add(1));
}

/// #13: the DEVELOPMENT HOST panel's aligned label/colon/value columns
/// — two-up rows (ip|user then pass alone); the longest label is
/// "passwd".
#[test]
fn test_host_panel_aligned_columns() {
    let state = create_state(&quick());
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    let host_rows: Vec<&RowSpec> = map
        .host_rows
        .iter()
        .filter(|r| {
            matches!(
                &r.key,
                Some(FieldKey::HostIp(_))
                    | Some(FieldKey::HostUser(_))
                    | Some(FieldKey::HostPass(_))
            )
        })
        .collect();
    assert_eq!(host_rows.len(), 3);
    // Column 1 (ip, user) and column 2 (pass) each share one
    // label x and one value x.
    let (col1, col2): (Vec<&RowSpec>, Vec<&RowSpec>) = host_rows
        .iter()
        .copied()
        .partition(|r| r.label_rect.x == host_rows[0].label_rect.x);
    assert_eq!(col1.len(), 2);
    assert_eq!(col2.len(), 1);
    let label_w = col1[0].label_rect.width;
    for col in [&col1, &col2] {
        let label_x = col[0].label_rect.x;
        let value_x = col[0].value_rect.x;
        for r in col {
            assert_eq!(
                r.label_rect.x, label_x,
                "label column left edge fixed per column"
            );
            assert_eq!(r.label_rect.width, label_w, "label column fixed width");
            assert_eq!(r.value_rect.x, value_x, "value column aligned per column");
        }
    }
    // The shared width is the longest label + 1 pad + 2 for the
    // required '* ' marker ("host name" = 9); the colon sits right
    // after the label column, the value two after the colon's
    // two-space gap.
    assert_eq!(label_w as usize, "host name".len() + 3);
    assert_eq!(col1[0].value_rect.x, col1[0].label_rect.x + label_w + 4);
    // The second column starts after the first pair's width + gap.
    let pair_w = map
        .host_panel
        .width
        .saturating_sub(2)
        .saturating_sub(4)
        .saturating_sub(2)
        .saturating_div(2);
    assert_eq!(col2[0].label_rect.x, col1[0].label_rect.x + pair_w + 2);
}

/// #14: the DUT content box —
/// The DUT content box is a LEFT/RIGHT two-column split (deliberate
///): ONE │ separator at the half mark at any width ≥60;
/// the field BOXES are pinned whole to columns (identity + relay LEFT,
/// flash + console + monitor RIGHT) and stack consecutively; each box
/// opens with a dim dashed TITLE rule (the titles ARE the rules — the
/// old between-group rule rows are gone); every column aligns its OWN
/// label width; all 26 relay-ON fields + the `supported:` hint are
/// present; the gate row is GONE (a chosen backend shows the relay
/// fields directly); the panel hugs the content exactly.
#[test]
fn test_dut_content_two_column_boxes_with_dashed_titles() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    ); // backend selected → relay fields show DIRECTLY (no gate row)
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let inner = map.dut_content_inner;
    // LEFT/RIGHT split: ONE separator at
    // the half mark, at any width ≥60.
    assert_eq!(map.separators.len(), 1);
    assert_eq!(map.separators[0].x, inner.x + inner.width / 2);
    let rect = |k: &FieldKey| {
        map.fields
            .iter()
            .find(|(f, _)| f == k)
            .unwrap_or_else(|| panic!("missing {k:?}"))
            .1
    };
    // The gate row is GONE; the relay sub-fields render directly.
    assert!(
        !map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayGate(0, 0)),
        "no gate row in the multi-column box"
    );
    assert_eq!(
        map.dut_rows.first().map(|r| r.label.as_str()),
        Some("Current DUT: dut1"),
        "title row stays line 1"
    );
    // Box integrity: every box's fields share ONE column and stack
    // consecutively top-to-bottom (the pinning never splits a box and
    // never reorders its fields).
    let col_of = |x: u16| {
        if x < map.separators[0].x { 0 } else { 1 }
    };
    for b in dut_boxes(&state, 0, 0) {
        let first = rect(&b.fields[0]);
        for (i, k) in b.fields.iter().enumerate() {
            let r = rect(k);
            assert_eq!(col_of(r.x), col_of(first.x), "{k:?} split across columns");
            assert_eq!(
                r.y,
                first.y.saturating_add(i as u16),
                "{k:?} not consecutive inside its box"
            );
        }
    }
    // Per-column label alignment: value x = label x + label width + 3
    // everywhere; every row in a column shares the column's width.
    for k in dut_boxes(&state, 0, 0).into_iter().flat_map(|b| b.fields) {
        let r = rect(&k);
        let row = map
            .dut_rows
            .iter()
            .find(|x| x.key.as_ref() == Some(&k))
            .unwrap();
        assert_eq!(
            r.x,
            row.label_rect
                .x
                .saturating_add(row.label_rect.width)
                .saturating_add(3),
            "{k:?} value must start after label + colon + space"
        );
    }
    // Content hugs: the pinned boxes pack into 17 rows (title + box
    // rows — the 5 dashed titles cost less than the 7 rule rows they
    // replace); the box interior is EXACTLY that
    // tall (no scrollbar, no blank rows) and the panel height follows.
    assert_eq!(map.dut_content_height, 18, "2-col relay-ON height");
    assert!(
        !map.dut_rows
            .iter()
            .any(|r| r.label.starts_with("supported:")),
        "the supported: hint is gone (options live in the picker popup)"
    );
    assert_eq!(
        map.dut_content_inner.height as usize, map.dut_content_height,
        "the box interior hugs the content"
    );
    assert_eq!(
        map.dut_panel.height as usize,
        map.dut_content_height + 4,
        "the DUT panel hugs its content (1 tab + 2 box borders + content + bottom border)"
    );
    // 80 cols → the SAME left/right split (one separator at the half
    // mark — deliberate: just split into left/right columns).
    let map80 = form_layout(Rect::new(0, 0, 80, 40), &state);
    assert_eq!(map80.separators.len(), 1);
    assert_eq!(
        map80.separators[0].x,
        map80.dut_content_inner.x + map80.dut_content_inner.width / 2
    );
    // 160 cols → the SAME left/right split at every width.
    let map160 = form_layout(Rect::new(0, 0, 160, 50), &state);
    assert_eq!(map160.separators.len(), 1);
    assert_eq!(
        map160.dut_content_height, map.dut_content_height,
        "the two-column height is width-independent"
    );
}

/// The DUT multi-column │ separator must clip to the content box and
/// scroll WITH the content — with a PINNED tall host panel the
/// relay-ON DUT content (18 rows) overflows its interior at 80x24,
/// and the separator must never paint over the panel borders
/// (the box titles ARE the horizontal rules, so there are
/// no rule rows and no ┼ crossings to paint anymore).
#[test]
fn test_dut_separator_clips_to_content_box_and_scrolls_with_content() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    ); // relay ON → 18 content rows at 80 cols
    state.host_h_pin = Some(8); // pin forces the DUT overflow at 80x24
    let map0 = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert!(
        map0.dut_content_height as u16 > map0.dut_content_inner.height,
        "the 80x24 relay-ON content overflows its interior"
    );
    assert_eq!(map0.separators.len(), 1, "80 cols → the left/right split");
    let sep_x = map0.separators[0].x;
    let cbox_bottom = map0.dut_content.bottom().saturating_sub(1);
    let panel_bottom = map0.dut_panel.bottom().saturating_sub(1);
    let terminal = terminal_with(&mut state, 80, 24);
    let buf = terminal.backend().buffer();
    // The separator never escapes the content box interior.
    assert_eq!(
        buf.content[buf.index_of(sep_x, cbox_bottom)].symbol(),
        "─",
        "content box bottom border must stay clean"
    );
    assert_eq!(
        buf.content[buf.index_of(sep_x, panel_bottom)].symbol(),
        "─",
        "DUT panel bottom border must stay clean"
    );
    // Scrolled: focus the LAST field so keep_focus_visible scrolls
    // the content — the separator shifts WITH the rows (the same
    // row_shift math) and the borders stay clean.
    state.focus = FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords"));
    let terminal = terminal_with(&mut state, 80, 24);
    let buf = terminal.backend().buffer();
    assert!(
        state.scroll > 0,
        "focusing the last field scrolls the overflowed content"
    );
    assert_eq!(
        buf.content[buf.index_of(sep_x, cbox_bottom)].symbol(),
        "─",
        "scrolled: content box bottom border still clean"
    );
    assert_eq!(
        buf.content[buf.index_of(sep_x, panel_bottom)].symbol(),
        "─",
        "scrolled: DUT panel bottom border still clean"
    );
}

/// #15: ONLY the selected host's panel and the selected DUT's fields
/// are laid out (one DUT's config at a time).
#[test]
fn test_dut_content_only_selected_duts_fields() {
    let mut state = create_state(&quick());
    state.add_dut(0); // host 0: 2 DUTs
    state.add_host(); // host 1 selected, 0 DUTs
    state.set_focus(FocusSlot::HostTab(0));
    assert_eq!(state.selected_host, 0);
    assert_eq!(state.selected_dut, Some(0));
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        state,
    );
    // Click dut2's tab → ONLY dut2's config shows.
    let m2 = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    let (_, d1) = m2.dut_tabs[1];
    tui.handle_mouse(mouse_down(d1.x + 2, d1.y)).unwrap();
    assert_eq!(tui.state.selected_dut, Some(1));
    let m3 = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    assert!(m3.fields.iter().any(|(k, _)| *k == FieldKey::DutPort(0, 1)));
    assert!(
        !m3.fields.iter().any(|(k, _)| *k == FieldKey::DutPort(0, 0)),
        "only the selected DUT's fields"
    );
    assert!(m3.fields.iter().any(|(k, _)| *k == FieldKey::HostIp(0)));
    assert!(
        !m3.fields.iter().any(|(k, _)| *k == FieldKey::HostIp(1)),
        "only the selected host's panel"
    );
}

// ── Render snapshots ───────────────────────────────────────────────

/// #16: the authoritative full-screen snapshot — host tabs with x/+,
/// panel titles, Current DUT, the dev_ctrl section rendering directly
/// with a chosen backend (the [x] (enabled) toggle row is gone —
///), footer chips; NO table borders, NO col_seps, NO left
/// sidebar, NO setup strip.
#[test]
fn test_render_snapshot_full_screen() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    ); // backend selected → relay sub-fields render directly
    // 100x56: the multi-column DUT content (2 columns at 100 cols, 18
    // rows with the relay section ON) needs enough interior rows for
    // "Current DUT" and the relay rows to paint.
    let terminal = terminal_with(&mut state, 100, 56);
    let rows = buffer_rows(&terminal);
    let joined = rows.join("\n");
    assert!(
        joined.contains("dutabo init — /proj/.target.jsonc"),
        "{joined}"
    );
    assert!(joined.contains("CREATE"), "{joined}");
    assert!(joined.contains("dev host 1  x"), "{joined}"); // tab card + its x
    assert!(joined.contains(" + "), "{joined}"); // trailing + cards
    assert!(joined.contains("DEVELOPMENT HOST"), "{joined}");
    assert!(joined.contains("DUTs"), "{joined}");
    assert!(joined.contains("Current DUT: dut1"), "{joined}");
    assert!(joined.contains("── identity"), "{joined}"); // box title rows
    assert!(joined.contains("── relay"), "{joined}");
    assert!(joined.contains("── flash"), "{joined}");
    assert!(joined.contains("reset"), "{joined}");
    assert!(
        !joined.contains("[x] (enabled)"),
        "the toggle row is gone: {joined}"
    );
    // Footer key hints are HIDDEN by default; [Save] is always there.
    assert!(
        !joined.contains("q: quit"),
        "hints hidden by default: {joined}"
    );
    assert!(joined.contains("[Save]"), "{joined}");
    // Deep-navy palette mapping (§5): background #061424, panel bg
    // #07182A, CURRENT tab = TAB_ACTIVE, title = ACCENT, chip surface.
    let buf = terminal.backend().buffer();
    let map = form_layout(Rect::new(0, 0, 100, 56), &state);
    let bg = &buf.content[buf.index_of(0, 0)];
    assert_eq!(bg.bg, BACKGROUND, "whole-screen background");
    let bar = &buf.content[buf.index_of(map.host_tab_bar.x + 30, map.host_tab_bar.y)];
    assert_eq!(bar.bg, PANEL_BG, "tab-bar surface");
    let tab = &buf.content[buf.index_of(map.host_tabs[0].1.x, map.host_tabs[0].1.y)];
    assert_eq!(tab.bg, TAB_ACTIVE, "CURRENT host tab highlighted");
    let title = &buf.content[buf.index_of(map.host_panel.x + 1, map.host_panel.y)];
    assert_eq!(title.fg, ACCENT, "bright-blue panel title");
    // '?' reveals the bordered key-hint chips (120 cols so all seven
    // fit beside [Save]).
    state.footer_hints_visible = true;
    let terminal = terminal_with(&mut state, 120, 30);
    let joined2 = buffer_rows(&terminal).join("\n");
    for hint in [
        "q: quit",
        "Tab/Shift-Tab focus",
        "←/→ switch tab",
        "Enter edit",
        "F2 save",
        "Del delete",
        "Esc quit",
    ] {
        assert!(joined2.contains(hint), "hint visible after '?': {joined2}");
    }
    let buf = terminal.backend().buffer();
    let map2 = form_layout(Rect::new(0, 0, 120, 30), &state);
    let chip = &buf.content[buf.index_of(map2.footer.x, map2.footer.y)];
    assert_eq!(chip.bg, SURFACE0, "footer chip surface");
    assert_eq!(chip.symbol(), "│", "footer chip thin left border");
    assert_eq!(chip.fg, OVERLAY1, "footer chip border glyph color");
    // NO table chrome / setup strip / footer buttons / left sidebar.
    assert!(!joined.contains("+ add host"), "{joined}");
    assert!(!joined.contains("dev hosts:"), "{joined}");
    assert!(!joined.contains("[Submit]"), "{joined}");
    assert!(!joined.contains("use relay?"), "{joined}");
}

/// #17: the advanced section is EXPANDED by default ([Other ▲] and the
/// advanced fields render INSIDE the host panel); [Other ▼] collapses.
#[test]
fn test_render_other_expanded_snapshot() {
    let mut state = create_state(&InitOptions::default());
    assert!(state.other_expanded, "expanded by default");
    state.scroll = 10; // the expanded advanced block scrolls (clamped)
    let terminal = terminal_with(&mut state, 120, 40);
    let rows = buffer_rows(&terminal);
    let joined = rows.join("\n");
    assert!(joined.contains("[Other ▲]"), "{joined}");
    assert!(joined.contains("serial.baudrate"), "{joined}");
    assert!(joined.contains("monitor.hang_timeout"), "{joined}");
    // Collapse: [Other ▼] and no advanced fields.
    state.toggle_other();
    let terminal = terminal_with(&mut state, 120, 40);
    let rows = buffer_rows(&terminal);
    let joined = rows.join("\n");
    assert!(joined.contains("[Other ▼]"), "{joined}");
}

/// The loader Select's "uefi" choice renders the English "Not
/// implemented" stub in the value cell (the picker still lists
/// uboot/uefi); "uboot" renders its normal option text instead.
#[test]
fn test_loader_uefi_renders_not_implemented_stub() {
    let mut state = create_state(&quick());
    state.set_value(FieldKey::DutAdvanced(0, 0, "loader"), "uefi".into());
    let terminal = terminal_with(&mut state, 120, 64);
    let joined = buffer_rows(&terminal).join("\n");
    // The field shows the raw value "uefi" (like dev_ctrl) and the
    // "not implemented" note renders BELOW it.
    assert!(joined.contains("uefi"), "the field shows uefi: {joined}");
    assert!(joined.contains("Not implemented"), "{joined}");
    // "uboot" (the default) renders its own text — no stub note.
    state.set_value(FieldKey::DutAdvanced(0, 0, "loader"), "uboot".into());
    let terminal = terminal_with(&mut state, 120, 64);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(!joined.contains("Not implemented"), "{joined}");
    assert!(joined.contains("uboot"), "{joined}");
}

// ── Keymap ─────────────────────────────────────────────────────────

/// Up/Down navigate fields WITHIN the focused zone only (host fields,
/// or the selected DUT's fields) — never the tabs or buttons.
#[test]
fn test_up_down_navigates_fields_only() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    // Up wraps to the LAST host-zone field (the zone is ip/user/pass/name).
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostName(0)));
    handle_form_key(&mut state, key(KeyCode::Down));
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
    for _ in 0..20 {
        handle_form_key(&mut state, key(KeyCode::Down));
    }
    assert!(!matches!(
        state.focus,
        FocusSlot::Button(_) | FocusSlot::HostTab(_) | FocusSlot::DutTab(_)
    ));
    // The DUT zone cycles the selected DUT's fields only.
    state.focus = FocusSlot::Field(FieldKey::DutName(0, 0));
    handle_form_key(&mut state, key(KeyCode::Down));
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::DutPort(0, 0)));
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::DutName(0, 0)));
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(
        state.focus,
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "monitor.crash_keywords")),
        "wraps within the DUT zone — the last DUT-zone field is now \
             monitor.crash_keywords (the DUT-owned advanced tail joined \
             the zone)"
    );
}

#[test]
fn test_printable_jkhl_reach_focused_text_field() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    for c in "hjkl".chars() {
        handle_form_key(&mut state, chr(c));
    }
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("hjkl"),
        "h/j/k/l must be inserted, not consumed as navigation"
    );
    assert_eq!(
        state.editors.get(&FieldKey::HostIp(0)).copied().unwrap().0,
        4,
        "cursor advances with every inserted char"
    );
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
}

#[test]
fn test_printable_jk_reach_focused_select_field_free_text() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::DutRelayType(0, 0));
    handle_form_key(&mut state, chr('k'));
    handle_form_key(&mut state, chr('j'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        None,
        "STRICT select: printable chars never enter the buffer "
    );
    assert_eq!(
        state.focus,
        FocusSlot::Field(FieldKey::DutRelayType(0, 0)),
        "focus stays on the select field"
    );
    handle_form_key(&mut state, key(KeyCode::Down));
    let options: Vec<String> = state
        .fields
        .iter()
        .find(|f| f.key == FieldKey::DutRelayType(0, 0))
        .unwrap()
        .options
        .clone();
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[0].as_str()),
        "Down still walks the options"
    );
}

#[test]
fn test_tab_and_ctrl_emacs_navigation_still_works() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    handle_form_key(&mut state, tab());
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostUser(0)));
    handle_form_key(&mut state, key(KeyCode::BackTab));
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
    type_into(&mut state, FieldKey::HostIp(0), "abc");
    handle_form_key(&mut state, ctrl('a'));
    handle_form_key(&mut state, ctrl('f'));
    assert_eq!(
        state.editors.get(&FieldKey::HostIp(0)).copied().unwrap().0,
        1,
        "Ctrl-f moves the cursor one cell right inside the editor"
    );
    handle_form_key(&mut state, ctrl('k'));
    assert_eq!(state.kill, "bc", "Ctrl-k kills to end of line");
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("a")
    );
    assert_eq!(state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
}

// ── Relay section follows the backend choice ──────────

/// The dev_ctrl section follows the BACKEND choice directly (the gate
/// toggle is gone): a chosen backend shows the relay sub-fields in the
/// layout and zone walks immediately; choosing none/disabled hides the
/// whole section while the relay buffers survive (per-DUT).
#[test]
fn test_relay_section_follows_backend_selection() {
    let mut state = create_state(&quick());
    let ctrl = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    // Probe the BOX model's flat order (the field_order walk follows
    // it — see test_dut_boxes_none_backend_keeps_dev_ctrl_only_relay_box).
    let has_relay_fields = |s: &FormState| {
        dut_boxes(s, 0, 0)
            .into_iter()
            .flat_map(|b| b.fields)
            .any(|k| k == FieldKey::DutRelayPowerOffTimeMs(0, 0))
    };
    // CREATE default: the backend is UNCHOSEN (empty) — the section
    // shows with empty sub-fields; none/disabled is the hiding choice.
    assert!(
        has_relay_fields(&state),
        "unchosen backend shows the section"
    );
    assert!(
        !state.field_order().contains(&FieldKey::DutRelayGate(0, 0)),
        "the gate row is gone from the validation walk"
    );
    // Type into a relay sub-field, then pick a backend through the
    // controller picker: a chosen backend enables the rows; the
    // separate type row stays gone.
    type_into(&mut state, FieldKey::DutRelayReset(0, 0), "3");
    state.set_focus(FocusSlot::Field(ctrl.clone()));
    handle_form_key(&mut state, chr(' ')); // Space opens the picker
    handle_form_key(&mut state, key(KeyCode::Enter)); // confirm ch340-relay
    assert_eq!(
        state.values.get(&ctrl).map(String::as_str),
        Some("ch340-relay")
    );
    assert!(
        has_relay_fields(&state),
        "a chosen backend keeps the section"
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert!(
        !map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayType(0, 0)),
        "the separate type row is gone"
    );
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayResetTimeMs(0, 0)),
        "relay rows render with a backend"
    );
    // Switch to none (the LAST of the two backend options)
    // through the picker: the section hides and the buffers survive.
    handle_form_key(&mut state, chr(' ')); // opens the picker
    handle_form_key(&mut state, key(KeyCode::Down));
    handle_form_key(&mut state, key(KeyCode::Enter)); // picks none
    assert_eq!(state.values.get(&ctrl).map(String::as_str), Some("none"));
    // has_relay_fields probes dut_boxes (slots stay reserved); the
    // blank-vs-visible distinction is the renderer's job.
    assert!(
        has_relay_fields(&state),
        "row slots stay reserved under none"
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayReset(0, 0))
            .map(String::as_str),
        Some("3"),
        "hidden relay buffers survive"
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayReset(0, 0)),
        "row slots stay reserved while the backend is none (no jump)"
    );
    assert!(
        !state
            .dut_zone_fields()
            .contains(&FieldKey::DutRelayReset(0, 0)),
        "blank rows are not focusable under none"
    );
}

#[test]
fn classify_legacy_relay_type_normalizes_only_usb_relay() {
    assert_eq!(normalize_relay_type("usb-relay"), "ch340-relay");
    assert_eq!(normalize_relay_type(" USB-RELAY "), "ch340-relay");
    // Unknown and current names pass through byte-for-byte: no silent
    // case rewriting beyond the one retired alias.
    assert_eq!(normalize_relay_type("ch340"), "ch340");
    assert_eq!(normalize_relay_type("ch340-relay"), "ch340-relay");
    assert_eq!(normalize_relay_type("none"), "none");
    assert_eq!(normalize_relay_type("mystery"), "mystery");
}

#[test]
fn test_relay_gate_prefills_yes_from_existing_relay() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "",
      "pass": "",
      "duts": [
        {
          "dut_name": "dut1",
          "serial": { "port": 41002 },
          "relay": { "type": "usb-relay", "port": 2011, "reset_ch": 2 }
        },
        { "dut_name": "dut2", "serial": { "port": 41003 } }
      ]
    }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let state = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayGate(0, 0))
            .map(String::as_str),
        Some("yes")
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        // The legacy `usb-relay` backend no longer exists; seeding must
        // repair it to the surviving equivalent, not round-trip a value
        // that config validation rejects at server start.
        Some("ch340-relay")
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayPort(0, 0))
            .map(String::as_str),
        Some("2011")
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayReset(0, 0))
            .map(String::as_str),
        Some("2")
    );
    assert!(
        !dut_ctrl_disabled(&state, 0, 0),
        "dut1 carries a relay section → its dev_ctrl section shows"
    );
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayGate(0, 1))
            .map(String::as_str),
        Some("no")
    );
    assert!(
        !dut_ctrl_disabled(&state, 0, 1),
        "dut2's unchosen backend still shows the (empty) section"
    );
}

// ── DUT-needs-host gating ──────────────────────────────────────────

/// Removing the last host hides EVERY DUT control; the save resolves to
/// ZERO hosts (so no DUT data can be written).
#[test]
fn test_zero_hosts_hides_dut_controls() {
    let mut state = create_state(&quick());
    state.remove_host(0);
    assert_eq!(state.host_count, 0);
    assert!(state.dut_counts.is_empty());
    assert!(state.field_order().is_empty());
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert!(
        !map.fields
            .iter()
            .any(|(k, _)| matches!(k, FieldKey::DutPort(..) | FieldKey::DutName(..))),
        "no DUT fields in the layout"
    );
    assert!(map.buttons.iter().any(|(b, _)| *b == ButtonId::AddHost));
    let answers = resolve_answers(
        &state.schema,
        &state.submit_values(),
        state.gate,
        NamePolicy::ErrorOnCollision,
    )
    .unwrap();
    assert!(answers.hosts.is_empty(), "{answers:?}");
    assert_eq!(
        state.focus_order(),
        vec![
            FocusSlot::Button(ButtonId::AddHost),
            FocusSlot::Button(ButtonId::Save),
            FocusSlot::Button(ButtonId::Test),
            FocusSlot::Button(ButtonId::SaveExit),
        ]
    );
}

// ── Select fields / editor ─────────────────────────────────────────

#[test]
fn test_select_field_cycle_digit_jump_free_text() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::DutRelayType(0, 0));
    let options: Vec<String> = state
        .fields
        .iter()
        .find(|f| f.key == FieldKey::DutRelayType(0, 0))
        .unwrap()
        .options
        .clone();
    assert!(!options.is_empty());
    handle_form_key(&mut state, key(KeyCode::Down));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[0].as_str())
    );
    handle_form_key(&mut state, key(KeyCode::Down));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[1].as_str())
    );
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[0].as_str())
    );
    handle_form_key(&mut state, chr('3'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[2].as_str())
    );
    handle_form_key(&mut state, chr('x'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[2].as_str()),
        "STRICT select: 'x' is ignored — the value stays on the option"
    );
    handle_form_key(&mut state, chr('y'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[2].as_str()),
        "STRICT select: 'y' is ignored too — the value never changes"
    );
}

#[test]
fn test_select_field_digits_insert_during_free_text() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::DutRelayType(0, 0));
    let options: Vec<String> = state
        .fields
        .iter()
        .find(|f| f.key == FieldKey::DutRelayType(0, 0))
        .unwrap()
        .options
        .clone();
    assert!(
        options.len() < 9,
        "the out-of-range digit case needs fewer than 9 options"
    );
    handle_form_key(&mut state, chr('1'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[0].as_str()),
        "empty buffer: digit 1 jumps to the first option"
    );
    handle_form_key(&mut state, chr('c'));
    handle_form_key(&mut state, chr('h'));
    handle_form_key(&mut state, chr('3'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[2].as_str()),
        "STRICT select: 'c'/'h' are ignored, digit 3 jumps to options[2]"
    );
    handle_form_key(&mut state, chr('4'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[3].as_str()),
        "STRICT select: digit 4 is the options[3] shortcut"
    );
    handle_form_key(&mut state, chr('0'));
    assert_eq!(
        state
            .values
            .get(&FieldKey::DutRelayType(0, 0))
            .map(String::as_str),
        Some(options[3].as_str()),
        "STRICT select: '0' is ignored — only options are settable"
    );
    assert_eq!(
        state
            .editors
            .get(&FieldKey::DutRelayType(0, 0))
            .copied()
            .unwrap()
            .0,
        options[3].chars().count(),
        "cursor sits at the picked option's end"
    );
    assert_eq!(
        state.focus,
        FocusSlot::Field(FieldKey::DutRelayType(0, 0)),
        "focus never moved"
    );
    // Out-of-range digit: silently ignored — the choice set is
    // CLOSED , nothing ever seeds the buffer.
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::DutRelayType(0, 0));
    let out = char::from_digit(options.len() as u32 + 1, 10).unwrap();
    handle_form_key(&mut state, chr(out));
    handle_form_key(&mut state, chr('r'));
    assert_eq!(
        state.values.get(&FieldKey::DutRelayType(0, 0)),
        None,
        "STRICT select: out-of-range digit and free chars are ignored"
    );
}

#[test]
fn test_emacs_editor_keys_shared_kill_buffer() {
    let mut state = create_state(&quick());
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    for c in "abc".chars() {
        handle_form_key(&mut state, chr(c));
    }
    handle_form_key(&mut state, ctrl('a'));
    handle_form_key(&mut state, ctrl('k'));
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("")
    );
    assert_eq!(state.kill, "abc");
    handle_form_key(&mut state, ctrl('y'));
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("abc")
    );
    handle_form_key(&mut state, alt('b'));
    assert_eq!(
        state.editors.get(&FieldKey::HostIp(0)).copied().unwrap().0,
        0
    );
    handle_form_key(&mut state, del());
    assert_eq!(
        state.values.get(&FieldKey::HostIp(0)).map(String::as_str),
        Some("bc"),
        "Del keeps the editor's delete-at-cursor on a FOCUSED FIELD"
    );
}

#[test]
fn test_submit_errors_focus_first_error_and_clear_on_edit() {
    let mut tui = test_tui();
    let res = tui.apply_action(FormAction::Submit).unwrap();
    assert!(res.is_none());
    assert_eq!(tui.state.errors.len(), 2, "{:?}", tui.state.errors);
    assert_eq!(tui.state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
    handle_form_key(&mut tui.state, chr('1'));
    assert!(!tui.state.errors.contains_key(&FieldKey::HostIp(0)));
    assert_eq!(tui.state.errors.len(), 1);
}

#[test]
fn test_page_up_page_down_scrolls_body() {
    let mut state = create_state(&InitOptions::default());
    state.scroll = 5;
    handle_form_key(&mut state, key(KeyCode::PageUp));
    assert_eq!(state.scroll, 0, "PageUp scrolls up (clamped)");
    handle_form_key(&mut state, key(KeyCode::PageDown));
    assert_eq!(state.scroll, 10);
    handle_form_key(&mut state, key(KeyCode::PageDown));
    assert_eq!(state.scroll, 20);
}

// ── Mouse (pure layout hit-testing) ────────────────────────────────

#[test]
fn test_mouse_click_adds_and_removes_grid_rows() {
    let mut tui = test_tui();
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let add_dut = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddDut(0))
        .unwrap()
        .1;
    let action = tui
        .handle_mouse(mouse_down(add_dut.x + 1, add_dut.y))
        .unwrap();
    assert_eq!(
        action,
        Some(FormAction::Continue),
        "grid clicks mutate and continue"
    );
    assert_eq!(tui.state.dut_counts[0], 2, "click added a DUT");
    tui.state.add_host();
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let rm_host = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::RemoveHost(0))
        .unwrap()
        .1;
    tui.handle_mouse(mouse_down(rm_host.x, rm_host.y)).unwrap();
    assert_eq!(tui.state.host_count, 1, "click removed host 0");
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let add_host = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddHost)
        .unwrap()
        .1;
    tui.handle_mouse(mouse_down(add_host.x + 1, add_host.y))
        .unwrap();
    assert_eq!(tui.state.host_count, 2, "click added a host tab");
}

/// Clicking the dev_ctrl Select row cycles the backend — the relay
/// sub-fields then appear DIRECTLY in the layout (the old
/// gate toggle row is gone; the backend choice IS the section switch).
#[test]
fn test_mouse_click_select_backend_shows_relay_fields_directly() {
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&quick()),
    );
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    let map = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    let (_, rect) = map.fields.iter().find(|(k, _)| *k == fk).unwrap();
    tui.handle_mouse(mouse_down(rect.x + 1, rect.y)).unwrap();
    assert!(
        tui.state.picker.is_some(),
        "clicking the select opens the picker"
    );
    // The picker carries the field's replaceable backend options
    // (ch340-relay and none only).
    let picker = tui.state.picker.as_ref().unwrap();
    assert_eq!(picker.options, vec!["ch340-relay", "none"]);
    // Confirm "none" via the picker: the sub-fields STAY in the layout
    // as dim placeholders — the box height never changes. "none" is the
    // LAST of the two replaceable backend options.
    handle_form_key(&mut tui.state, key(KeyCode::Down));
    handle_form_key(&mut tui.state, key(KeyCode::Enter));
    assert_eq!(
        tui.state.values.get(&fk).map(String::as_str),
        Some("none"),
        "picker Down+Enter selects none"
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayReset(0, 0)),
        "the row slot stays reserved under none (constant height)"
    );
    assert!(
        !tui.state
            .dut_zone_fields()
            .contains(&FieldKey::DutRelayReset(0, 0)),
        "blank rows are not focusable"
    );
}

/// ComboBox mouse semantics: clicking an option row picks
/// it and closes; hover moves the browse cursor; clicking the field
/// cell or anywhere outside closes without a choice. The popup
/// hit-test uses the SAME rect as the renderer.
#[test]
fn test_picker_mouse_click_option_hover_and_outside_close() {
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&quick()),
    );
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    let map = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    let (_, rect) = map.fields.iter().find(|(k, _)| *k == fk).unwrap();
    let (fx, fy) = (rect.x + 1, rect.y);
    // Click the select field → picker opens.
    tui.handle_mouse(mouse_down(fx, fy)).unwrap();
    assert!(tui.state.picker.is_some(), "click opens the picker");
    let area = Rect::new(0, 0, 120, 40);
    let map = form_layout(area, &tui.state);
    let popup = picker_popup_rect(&tui.state, &map, area).unwrap();
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    // Hover the LAST option row ("none", the 2nd of the replaceable
    // backend options) → the browse cursor follows (the
    // committed value stays untouched).
    tui.handle_mouse(mouse_move(inner.x + 2, inner.y + 1))
        .unwrap();
    assert_eq!(
        tui.state.picker.as_ref().unwrap().cursor,
        1,
        "hover moves the browse cursor"
    );
    // Click the last option → committed + closed.
    tui.handle_mouse(mouse_down(inner.x + 2, inner.y + 1))
        .unwrap();
    assert_eq!(
        tui.state.values.get(&fk).map(String::as_str),
        Some("none"),
        "clicking an option commits it"
    );
    assert!(tui.state.picker.is_none(), "clicking an option closes");
    // Reopen: clicking the field cell while open toggles closed.
    tui.handle_mouse(mouse_down(fx, fy)).unwrap();
    assert!(tui.state.picker.is_some(), "click reopens");
    tui.handle_mouse(mouse_down(fx, fy)).unwrap();
    assert!(
        tui.state.picker.is_none(),
        "clicking the field toggles closed"
    );
    // Open again and click far outside → closes without a choice.
    tui.handle_mouse(mouse_down(fx, fy)).unwrap();
    assert!(tui.state.picker.is_some());
    tui.handle_mouse(mouse_down(2, 2)).unwrap();
    assert!(tui.state.picker.is_none(), "clicking outside closes");
    // The committed value survived the cancel.
    assert_eq!(
        tui.state.values.get(&fk).map(String::as_str),
        Some("none"),
        "cancel keeps the committed value"
    );
}

/// #18: clicking a host tab's `x` deletes THAT host (its DUTs go with
/// it; the selected host clamps onto the survivor).
#[test]
fn test_click_host_tab_x_deletes_host() {
    let mut tui = test_tui();
    tui.state.add_host(); // dev host1 (dut1), dev host2 (selected)
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let (_, x0) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::RemoveHost(0))
        .unwrap();
    tui.handle_mouse(mouse_down(x0.x, x0.y)).unwrap();
    assert_eq!(tui.state.host_count, 1, "x deleted host 0");
    assert_eq!(tui.state.selected_host, 0, "selection clamped");
    assert_eq!(
        tui.state.dut_counts,
        vec![1],
        "the survivor keeps its seeded DUT"
    );
    assert_eq!(tui.state.focus, FocusSlot::HostTab(0));
}

/// #18: clicking a DUT tab's `x` deletes THAT DUT and clamps the
/// selection (the shifted-in DUT becomes current).
#[test]
fn test_click_dut_tab_x_deletes_dut() {
    let mut tui = test_tui();
    tui.state.add_dut(0); // dut1, dut2 — dut2 selected
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let (_, x0) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::RemoveDut(0, 0))
        .unwrap();
    tui.handle_mouse(mouse_down(x0.x, x0.y)).unwrap();
    assert_eq!(tui.state.dut_counts[0], 1, "x deleted dut1");
    assert_eq!(tui.state.selected_dut, Some(0), "selection clamped");
    assert_eq!(tui.state.focus, FocusSlot::DutTab(0));
}

/// #18: clicking the trailing `+` cards adds a host/DUT and
/// AUTO-SELECTS the new item (its tab becomes current).
#[test]
fn test_click_plus_adds() {
    let mut tui = test_tui();
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let (_, plus_host) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddHost)
        .unwrap();
    tui.handle_mouse(mouse_down(plus_host.x + 1, plus_host.y))
        .unwrap();
    assert_eq!(tui.state.host_count, 2, "+ added a host");
    assert_eq!(tui.state.selected_host, 1, "the new host is selected");
    assert_eq!(tui.state.focus, FocusSlot::HostTab(1));
    // Host 2 already carries its seeded dut1 — click + to add dut2.
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let (_, plus_dut) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddDut(1))
        .unwrap();
    tui.handle_mouse(mouse_down(plus_dut.x + 1, plus_dut.y))
        .unwrap();
    assert_eq!(tui.state.dut_counts[1], 2, "+ added dut2 to host 2");
    assert_eq!(tui.state.selected_dut, Some(1), "the new DUT is selected");
    assert_eq!(tui.state.focus, FocusSlot::DutTab(1));
}

/// #18: clicking `[Other ▼]` toggles the advanced section (collapsed ↔
/// expanded); the Confirm gate follows the expansion.
#[test]
fn test_click_other_toggle() {
    let mut tui = test_tui();
    // Default opts: advanced_gate = Confirm — the Other button exists.
    tui.state = create_state(&InitOptions::default());
    assert!(tui.state.other_expanded, "expanded by default");
    let map = form_layout(Rect::new(0, 0, 80, 24), &tui.state);
    let other = map.other_btn.expect("Confirm mode shows [Other ▲]");
    tui.handle_mouse(mouse_down(other.x + 4, other.y)).unwrap();
    assert!(
        !tui.state.other_expanded,
        "click collapsed the advanced fields"
    );
    assert!(!tui.state.gate);
    tui.handle_mouse(mouse_down(other.x + 4, other.y)).unwrap();
    assert!(tui.state.other_expanded, "click expanded again");
    assert!(tui.state.gate, "the gate followed the expansion");
}

/// #18: clicking a field focuses it and places the cursor at the click
/// column; clicking a DUT field makes that DUT current.
#[test]
fn test_click_field_focuses() {
    // 120x40 (was 80x24): the DUT content rows outgrow an 80x24 box —
    // serial.port's row would clip out of the box and the click
    // cannot land.
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&quick()),
    );
    tui.state
        .values
        .insert(FieldKey::HostIp(0), "10.0.0.1".into());
    let map = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    let (_, ip) = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::HostIp(0))
        .unwrap();
    tui.handle_mouse(mouse_down(ip.x + 3, ip.y)).unwrap();
    assert_eq!(tui.state.focus, FocusSlot::Field(FieldKey::HostIp(0)));
    assert_eq!(
        tui.state.editors.get(&FieldKey::HostIp(0)).map(|(c, _)| *c),
        Some(3),
        "cursor placed at the click column"
    );
    // A DUT field click selects that DUT and focuses the field.
    let map = form_layout(Rect::new(0, 0, 120, 40), &tui.state);
    let (_, port) = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::DutPort(0, 0))
        .unwrap();
    tui.handle_mouse(mouse_down(port.x + 1, port.y)).unwrap();
    assert_eq!(tui.state.focus, FocusSlot::Field(FieldKey::DutPort(0, 0)));
    assert_eq!(tui.state.selected_dut, Some(0));
}

// ── E2E flows (§7.1 #19-22: run_form_with + KeyQueue) ──────────────

/// #19 CREATE: click the host bar's `+` to add dev host2 → type its
/// ip → click host1's tab → fill host1's ip (every host's ip is
/// required) → click dut1's tab → edit serial.port → F2 → the file is
/// created (dut1 + the typed port), no .tmp residue, .mcp.json
/// reconciled.
#[test]
fn test_e2e_create_add_host_type_fields_click_dut_f2() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    // Click coordinates come from the SAME pure layout the form loop
    // builds; a probe state mirrors each handler mutation so every
    // click lands on the post-mutation layout.
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let mut probe = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    let mut events: Vec<Event> = Vec::new();
    let field_rect = |map: &LayoutMap, key: &FieldKey| -> Rect {
        map.fields.iter().find(|(k, _)| k == key).unwrap().1
    };
    // 1. Host-bar `+` → dev host2 (auto-selected, one seeded DUT).
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let (_, plus) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddHost)
        .unwrap();
    events.push(Event::Mouse(mouse_down(plus.x + 1, plus.y)));
    probe.add_host();
    // 2. Click host2's ip → type "10.0.0.2".
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let ip = field_rect(&map, &FieldKey::HostIp(1));
    events.push(Event::Mouse(mouse_down(ip.x + 1, ip.y)));
    probe.set_focus(FocusSlot::Field(FieldKey::HostIp(1)));
    events.extend(typed("10.0.0.2"));
    for c in "10.0.0.2".chars() {
        handle_form_key(&mut probe, chr(c));
    }
    // 3b. Click host2's SEEDED dut serial.port → type "41003" (required
    // for a clean F2 — add_host seeds one DUT).
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let port1 = field_rect(&map, &FieldKey::DutPort(1, 0));
    events.push(Event::Mouse(mouse_down(port1.x + 1, port1.y)));
    probe.set_focus(FocusSlot::Field(FieldKey::DutPort(1, 0)));
    events.extend(typed("41003"));
    for c in "41003".chars() {
        handle_form_key(&mut probe, chr(c));
    }
    // 4. Click host1's tab — BOTH panels switch back.
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let (_, tab0) = map.host_tabs[0];
    events.push(Event::Mouse(mouse_down(tab0.x + 2, tab0.y)));
    probe.set_focus(FocusSlot::HostTab(0));
    // 4b. Click host1's ip → type "10.0.0.1" (required for a clean F2).
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let ip0 = field_rect(&map, &FieldKey::HostIp(0));
    events.push(Event::Mouse(mouse_down(ip0.x + 1, ip0.y)));
    probe.set_focus(FocusSlot::Field(FieldKey::HostIp(0)));
    events.extend(typed("10.0.0.1"));
    for c in "10.0.0.1".chars() {
        handle_form_key(&mut probe, chr(c));
    }
    // 5. Click dut1's tab.
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let (_, dut0) = map.dut_tabs[0];
    events.push(Event::Mouse(mouse_down(dut0.x + 2, dut0.y)));
    probe.set_focus(FocusSlot::DutTab(0));
    // 6. Click dut1's serial.port → type "41002".
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let port = field_rect(&map, &FieldKey::DutPort(0, 0));
    events.push(Event::Mouse(mouse_down(port.x + 1, port.y)));
    probe.set_focus(FocusSlot::Field(FieldKey::DutPort(0, 0)));
    events.extend(typed("41002"));
    for c in "41002".chars() {
        handle_form_key(&mut probe, chr(c));
    }
    // 7. F2 saves DIRECTLY (no Review screen).
    events.push(ev(f2()));
    let outcome = run_form_with(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap();
    assert!(outcome.config_created);
    let text = std::fs::read_to_string(tmp.path().join(".target.jsonc")).unwrap();
    assert!(text.contains("\"dut_name\": \"dut1\""), "{text}");
    assert!(text.contains("\"ip\": \"10.0.0.1\""), "{text}");
    assert!(text.contains("\"ip\": \"10.0.0.2\""), "{text}");
    assert!(text.contains("41002"), "{text}");
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
    assert!(
        outcome.changes.iter().any(|c| c.contains(".mcp.json")),
        ".mcp.json reconciled: {:?}",
        outcome.changes
    );
}

/// #20 UPDATE: load a commented fixture → add DUT via the DUT bar's
/// `+` card → set the new DUT's serial.port → F2 → the file gains
/// dut2 while the untouched lines stay byte-identical (comment
/// preservation), no .tmp residue.
#[test]
fn test_e2e_update_add_dut_preserves_comments() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  // top comment — keep
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "",
      "pass": "",
      "duts": [
        { "dut_name": "dut1", "serial": { "port": 41002, "baudrate": 460800 } }
      ]
      // host tail comment — keep
    }
  ]
  // file tail comment — keep
}
"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    assert!(!prepared.created, "existing file → UPDATE");
    let mut probe = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    let mut events: Vec<Event> = Vec::new();
    // 1. The DUT bar's `+` → dut2 (auto-selected).
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let (_, plus) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::AddDut(0))
        .unwrap();
    events.push(Event::Mouse(mouse_down(plus.x + 1, plus.y)));
    probe.add_dut(0);
    // 2. Click dut2's serial.port → type "41003".
    let map = form_layout(Rect::new(0, 0, 120, 40), &probe);
    let port = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::DutPort(0, 1))
        .unwrap()
        .1;
    events.push(Event::Mouse(mouse_down(port.x + 1, port.y)));
    probe.set_focus(FocusSlot::Field(FieldKey::DutPort(0, 1)));
    events.extend(typed("41003"));
    for c in "41003".chars() {
        handle_form_key(&mut probe, chr(c));
    }
    // 3. F2 saves DIRECTLY.
    events.push(ev(f2()));
    let outcome = run_form_with(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap();
    assert!(!outcome.config_created);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"dut_name\": \"dut2\""), "{text}");
    assert!(text.contains("41003"), "{text}");
    // Comment preservation: every comment survives verbatim.
    assert!(text.contains("// top comment — keep"), "{text}");
    assert!(text.contains("// host tail comment — keep"), "{text}");
    assert!(text.contains("// file tail comment — keep"), "{text}");
    // Byte-identical untouched lines (append-only UPDATE): the prefix
    // through dut1's whole line is untouched, and every byte after the
    // duts array's closing `]` line is untouched. The append span runs
    // from the end of dut1's line through `]`: the inserted comma takes
    // over that line's indent and the block lands before `]` (the
    // AppendBlock op in init.rs — the closing bracket keeps its
    // position, the bytes after it are preserved verbatim).
    let cut = FIXTURE.find("}\n      ]").unwrap() + 1;
    assert!(
        text.starts_with(&FIXTURE[..cut]),
        "untouched prefix changed:\n{text}"
    );
    let tail = FIXTURE.split_once("\n      ]\n").unwrap().1;
    assert!(
        text.ends_with(tail),
        "untouched tail after the duts array changed:\n{text}"
    );
    assert!(
        text.contains("\n]\n      // host tail comment — keep"),
        "the closing bracket keeps its position right before the tail:\n{text}"
    );
    let parsed = sermcp::config::parse_jsonc_text(&text).expect("updated file parses");
    assert_eq!(parsed.dev_hosts.len(), 1);
    assert_eq!(parsed.dev_hosts[0].duts.len(), 2);
    assert_eq!(
        parsed.dev_hosts[0].duts[1]
            .serial
            .as_ref()
            .unwrap()
            .port
            .as_ref()
            .unwrap()
            .to_value_string(),
        "41003"
    );
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
}
/// #21 UPDATE no-op: F2 with the prefilled defaults → the file stays
/// byte-identical, no .tmp residue.
#[test]
fn test_e2e_update_noop_f2_keeps_file_byte_identical() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1",
      "user": "root",
      "pass": "",
      "duts": [
        {
          "dut_name": "dut1",
          "serial": { "port": 41002, "baudrate": 460800 },
          "target": { "login_user": "root" }
        }
      ]
    }
  ]
}
"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let outcome = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(vec![ev(f2())]),
        &quick(),
        tmp.path(),
    )
    .unwrap();
    assert!(!outcome.config_created);
    assert!(
        outcome.changes.iter().any(|c| c.contains("unchanged")),
        "{:?}",
        outcome.changes
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        FIXTURE,
        "no-op F2 must leave the file byte-identical"
    );
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
}

/// #22 ABORT: Esc right after edits → zero bytes written (no config,
/// no .tmp, no .mcp.json).
#[test]
fn test_e2e_esc_abort_writes_zero_bytes() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    // CREATE: focus starts on HostTab(0); two Tabs reach HostIp(0).
    let mut events: Vec<Event> = Vec::new();
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.extend(typed("10.0.0.1"));
    events.push(ev(esc()));
    let err = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap_err();
    assert!(err.contains("aborted by user"), "{err}");
    assert!(!tmp.path().join(".target.jsonc").exists());
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
    assert!(!tmp.path().join(".mcp.json").exists());
}

/// #22b ABORT via 'q': same zero-byte guarantee, driven by the
/// context-sensitive quit — after mid-edit typing, one Tab leaves the
/// non-empty ip buffer (focus moves to the empty user field) and 'q'
/// quits immediately.
#[test]
fn test_e2e_q_abort_writes_zero_bytes() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    // CREATE: focus starts on HostTab(0); two Tabs reach HostIp(0);
    // type (its buffer becomes non-empty), Tab → HostUser(0) (empty),
    // then 'q' quits.
    let mut events: Vec<Event> = Vec::new();
    events.push(ev(tab()));
    events.push(ev(tab()));
    events.extend(typed("10.0.0.1"));
    events.push(ev(tab()));
    events.push(ev(chr('q')));
    let err = run_form_with(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(events),
        &quick(),
        tmp.path(),
    )
    .unwrap_err();
    assert!(err.contains("aborted by user"), "{err}");
    assert!(!tmp.path().join(".target.jsonc").exists());
    assert!(!tmp.path().join(".target.jsonc.tmp").exists());
    assert!(!tmp.path().join(".mcp.json").exists());
}

/// INPUT AFFORDANCE: the focused field renders the reversed cursor
/// cell, bold + underlined text on a distinct background painted
/// across the WHOLE value rect; unfocused fields keep the quiet look.
#[test]
fn test_render_input_field_affordance_and_focused_state() {
    let mut state = create_state(&quick());
    state.values.insert(FieldKey::HostIp(0), "10.0.0.1".into());
    state.editors.insert(FieldKey::HostIp(0), (3, 0));
    state.focus = FocusSlot::Field(FieldKey::HostIp(0));
    // 120x40 (was 80x24): the unfocused serial.port row must actually
    // paint for the background/underline checks — at 80x24 the DUT
    // content clips it out of the box.
    let terminal = terminal_with(&mut state, 120, 40);
    let buf = terminal.backend().buffer();
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let ip = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::HostIp(0))
        .unwrap()
        .1;
    let cur = &buf.content[buf.index_of(ip.x + 3, ip.y)];
    assert!(
        cur.modifier.contains(Modifier::REVERSED),
        "focused cursor visible"
    );
    let first = &buf.content[buf.index_of(ip.x, ip.y)];
    assert!(first.modifier.contains(Modifier::BOLD), "focused text bold");
    assert!(
        first.modifier.contains(Modifier::UNDERLINED),
        "focused text underlined"
    );
    assert_ne!(first.bg, Color::Reset, "focused field background");
    for x in ip.x..ip.right() {
        let cell = &buf.content[buf.index_of(x, ip.y)];
        assert_eq!(cell.bg, first.bg, "bg uniform across the value rect");
    }
    let port = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::DutPort(0, 0))
        .unwrap()
        .1;
    let p = &buf.content[buf.index_of(port.x, port.y)];
    assert_ne!(p.bg, Color::Reset, "unfocused field has a background");
    assert!(
        p.modifier.contains(Modifier::UNDERLINED),
        "unfocused field underlined"
    );
    assert_ne!(p.bg, first.bg, "focused vs unfocused backgrounds differ");
}

// ── scroll/geometry primitives ─────────────────────────────────────

#[test]
fn test_shift_rect_keeps_bottom_slice_of_partially_scrolled_blocks() {
    let clip = Rect {
        x: 0,
        y: 3,
        width: 80,
        height: 18,
    };
    let el = Rect {
        x: 1,
        y: 6,
        width: 40,
        height: 5,
    };
    assert_eq!(
        shift_rect(el, 0, clip),
        Some(Rect {
            x: 1,
            y: 6,
            width: 40,
            height: 5
        })
    );
    assert_eq!(
        shift_rect(el, 4, clip),
        Some(Rect {
            x: 1,
            y: 3,
            width: 40,
            height: 4
        })
    );
    assert_eq!(
        shift_rect(el, 7, clip),
        Some(Rect {
            x: 1,
            y: 3,
            width: 40,
            height: 1
        })
    );
    assert_eq!(shift_rect(el, 10, clip), None);
    assert_eq!(
        shift_rect(el, 1, clip),
        Some(Rect {
            x: 1,
            y: 5,
            width: 40,
            height: 5
        })
    );
}

/// The expanded advanced block clips to the HOST panel even when it
/// overflows below it — overflow rows must never paint into the DUT
/// content box (row_shift must classify rows by their OWNING container,
/// not by their absolute Y position).
/// the host advanced block shrank to the 4 HOST-LEVEL keys
/// while the 16 DUT-owned keys moved to the DUT box — a pinning drag
/// forces the overflow so the clip is still exercised.
#[test]
fn test_expanded_advanced_overflow_stays_inside_host_panel() {
    let mut state = create_state(&InitOptions::default());
    assert!(state.other_expanded, "expanded by default");
    // Pin the host panel down to 4 rows: the expanded advanced block
    // (4 two-up content rows) overflows below the panel — the host
    // panel scrolls (host_scrollbar) and the overflow must never
    // paint into the DUT content box.
    state.host_h_pin = Some(4);
    let terminal = terminal_with(&mut state, 120, 40);
    let rows = buffer_rows(&terminal);
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert!(
        map.host_content_height as u16 > map.host_panel_inner.height,
        "the pinned host panel overflows"
    );
    assert!(map.host_scrollbar.is_some(), "the host panel scrolls first");
    let title_row = rows[map.dut_content_inner.y as usize].clone();
    let row: String = title_row
        .chars()
        .skip(map.dut_content_inner.x as usize + 2)
        .take((map.dut_content_inner.width.saturating_sub(4)) as usize)
        .collect();
    assert!(
        row.trim_end().ends_with("Current DUT: dut1"),
        "DUT title row must be clean: {row:?}"
    );
    assert!(
        !row.contains(":  ["),
        "advanced colon/bracket leaked: {row:?}"
    );
    // The expanded host rows are the base four + the 4 host-level keys
    // (the moved DUT-owned keys stay out of the host panel).
    let host_adv: Vec<&str> = map
        .host_rows
        .iter()
        .filter_map(|r| match &r.key {
            Some(FieldKey::Advanced(k)) => Some(*k),
            _ => None,
        })
        .collect();
    assert_eq!(host_adv.len(), 4, "the shrunken host-level advanced set");
    for moved in [
        "serial.baudrate",
        "control.dev_ctrl",
        "target.login_pass",
        "target.login_prompt",
        "uboot.interrupt_char",
        "uboot.interrupt_strategy",
        "flash.tool",
        "monitor.reference_log",
        "monitor.max_archived_logs",
        "monitor.crash_keywords",
        "loader",
    ] {
        assert!(
            !host_adv.contains(&moved),
            "the DUT-owned key {moved} must be ABSENT from the host rows"
        );
    }
}

#[test]
fn test_render_small_terminal_no_panic() {
    for (w, h) in [(80, 24), (40, 10), (30, 8)] {
        let mut state = create_state(&InitOptions::default());
        let terminal = terminal_with(&mut state, w, h);
        let _ = buffer_rows(&terminal);
    }
    let mut host_state = create_state(&quick());
    host_state.add_dut(0);
    host_state.add_dut(0);
    host_state.add_dut(0);
    let terminal = terminal_with(&mut host_state, 40, 10);
    let _ = buffer_rows(&terminal);
}

/// The form transport's `input`/`confirm` are unreachable by contract.
#[test]
fn test_form_transport_asks_no_questions() {
    let mut tui = test_tui();
    assert_eq!(
        tui.input("x", None).unwrap_err(),
        "form transport asks no questions"
    );
    assert_eq!(
        tui.confirm("x", false).unwrap_err(),
        "form transport asks no questions"
    );
}

/// At heights below 20 the four-zone chunk layout cannot fit BOTH
/// panels' interiors (at 40x10 the host panel and the DUT content box
/// have 0 interior rows — an empty, unusable form), so the gate falls
/// back to the plain prompt flow instead. At the floor the host panel
/// shows its rows and the DUT content box keeps at least its title row
/// —: the DUT content now carries the DUT-owned advanced
/// boxes (18+ rows), so the DUT panel's cramped-floor reserve was
/// raised (DUT_MIN_ABS 5→6) to keep ONE interior row for the DUT box
/// where 5 left it borders-only.
#[test]
fn test_minimum_usable_height_keeps_panels_visible() {
    assert!(!terminal_usable((40, 10)), "40x10 renders an empty form");
    assert!(!terminal_usable((80, 19)));
    assert!(terminal_usable((40, 20)));
    assert!(terminal_usable((80, 24)));
    let mut state = create_state(&quick());
    let terminal = terminal_with(&mut state, 40, 20);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(joined.contains("dev host 1"), "{joined}");
    assert!(joined.contains("Current DUT"), "{joined}");
}

#[test]
fn test_size_gate_rejects_tiny_terminals() {
    assert!(terminal_usable((40, 20)));
    assert!(terminal_usable((200, 60)));
    assert!(!terminal_usable((39, 20)));
    assert!(!terminal_usable((40, 19)));
    assert!(!terminal_usable((0, 0)));
}

#[test]
fn test_tui_allowed_decision() {
    assert!(tui_allowed(true, true, "xterm-256color", (80, 24)));
    assert!(tui_allowed(true, true, "", (80, 24))); // TERM unset is fine
    assert!(!tui_allowed(false, true, "xterm", (80, 24))); // piped stdin
    assert!(!tui_allowed(true, false, "xterm", (80, 24))); // piped stdout
    assert!(!tui_allowed(true, true, "dumb", (80, 24)));
    assert!(!tui_allowed(true, true, "DUMB", (80, 24)));
    assert!(!tui_allowed(true, true, "xterm", (20, 20)));
}

// ── Host panel field changes ─────────────────────────

/// add_host seeds ONE default DUT (auto-selected) — deliberate
/// new dev hosts must not start with an empty DUT area.
#[test]
fn test_add_host_creates_one_dut_by_default() {
    let mut state = create_state(&InitOptions::default());
    state.add_host();
    assert_eq!(state.host_count, 2);
    assert_eq!(state.dut_counts[1], 1, "new host seeds one DUT");
    assert_eq!(state.selected_dut, Some(0), "seeded DUT auto-selected");
}

/// The advanced ([Other]) section is expanded by DEFAULT.
#[test]
fn test_other_expanded_by_default() {
    let state = create_state(&InitOptions::default());
    assert!(state.other_expanded, "advanced section expanded by default");
}

/// Host panel: ip/user/pass/host name in the two-up layout (ip|user on
/// one row, pass|host name below); target.login_prompt always visible
/// in the DUT box and NOT duplicated in the advanced block.
#[test]
fn test_host_panel_two_columns_and_login_prompt_placement() {
    let mut state = create_state(&InitOptions::default());
    state.other_expanded = false; // isolate the base fields
    let area = Rect::new(0, 0, 100, 40);
    let map = form_layout(area, &state);
    let ip = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::HostIp(0))
        .expect("ip field");
    let usr = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::HostUser(0))
        .expect("user field");
    let pass = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::HostPass(0))
        .expect("pass field");
    let name = map
        .fields
        .iter()
        .find(|(k, _)| *k == FieldKey::HostName(0))
        .expect("host name field");
    assert_eq!(ip.1.y, usr.1.y, "ip and user share one row");
    assert!(usr.1.x > ip.1.x, "user sits right of ip");
    assert_eq!(
        pass.1.y, name.1.y,
        "pass pairs with host_name on the row below"
    );
    assert!(name.1.x > pass.1.x, "host_name sits right of pass");
    // login_prompt lives in the DUT content box (right column, under
    // login pass), NOT the host panel.
    assert!(
        map.dut_rows
            .iter()
            .any(|r| r.key.as_ref() == Some(&FieldKey::DutAdvanced(0, 0, "target.login_prompt"))),
        "login_prompt shown in the DUT box"
    );
    assert!(
        !map.host_rows
            .iter()
            .any(|r| r.key.as_ref() == Some(&FieldKey::DutAdvanced(0, 0, "target.login_prompt"))),
        "login_prompt is NOT in the host panel"
    );
    // and is NOT repeated anywhere else: login_prompt is DUT-owned
    // — it appears exactly once, in the DUT box.
    state.other_expanded = true;
    let map2 = form_layout(area, &state);
    let host_login_prompt = map2
        .fields
        .iter()
        .filter(|(k, _)| matches!(k, FieldKey::Advanced(a) if *a == "target.login_prompt"))
        .count();
    assert_eq!(
        host_login_prompt, 0,
        "login_prompt is DUT-owned — never in the host panel"
    );
    let dut_login_prompt = map2
        .fields
        .iter()
        .filter(|(k, _)| *k == FieldKey::DutAdvanced(0, 0, "target.login_prompt"))
        .count();
    assert_eq!(
        dut_login_prompt, 1,
        "login_prompt appears exactly once (the DUT box)"
    );
}

// ── Tab names follow the typed names ─────────────────

/// Tabs show the TYPED name once the field is filled (default
/// 'dev host N' / 'dut N' when empty) — deliberate.
#[test]
fn test_tab_names_follow_typed_names() {
    let mut state = create_state(&quick());
    type_into(&mut state, FieldKey::DutName(0, 0), "uart0");
    let terminal = terminal_with(&mut state, 120, 40);
    let rows = buffer_rows(&terminal);
    let joined = rows.join("\n");
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let dut_row = &rows[map.dut_tab_bar.y as usize];
    assert!(
        dut_row.contains("uart0"),
        "dut tab shows the typed name: {dut_row}"
    );
    assert!(!dut_row.contains("dut 1"), "{dut_row}");
    assert!(joined.contains("Current DUT: uart0"), "{joined}");
    // Typing a host_name renames the host tab to that name (24-char cap).
    let mut state_named = create_state(&quick());
    type_into(&mut state_named, FieldKey::HostName(0), "lab-pc");
    let terminal_n = terminal_with(&mut state_named, 120, 40);
    let rows_n = buffer_rows(&terminal_n);
    let map_n = form_layout(Rect::new(0, 0, 120, 40), &state_named);
    assert!(
        rows_n[map_n.host_tab_bar.y as usize].contains("lab-pc"),
        "host tab shows the typed host_name"
    );
    // Empty values fall back to the defaults.
    let mut state2 = create_state(&quick());
    let terminal2 = terminal_with(&mut state2, 120, 40);
    let rows2 = buffer_rows(&terminal2);
    let map2 = form_layout(Rect::new(0, 0, 120, 40), &state2);
    assert!(rows2[map2.host_tab_bar.y as usize].contains("dev host 1"));
    assert!(rows2[map2.dut_tab_bar.y as usize].contains("dut1"));
}

// ── Save button + hidden footer hints ────────────────

/// [Save] sits at the footer's right edge and submits on click/Enter;
/// the key-hint chips are HIDDEN by default, '?' toggles them.
#[test]
fn test_save_button_and_footer_hints_hidden_by_default() {
    let mut state = create_state(&quick());
    assert!(!state.footer_hints_visible, "hints hidden by default");
    let map = form_layout(Rect::new(0, 0, 100, 30), &state);
    assert!(
        map.buttons.iter().any(|(b, _)| *b == ButtonId::Save),
        "save button exists"
    );
    // '?' toggles the hints (from a non-text focus).
    state.set_focus(FocusSlot::HostTab(0));
    handle_form_key(&mut state, chr('?'));
    assert!(state.footer_hints_visible, "'?' reveals the hints");
    handle_form_key(&mut state, chr('?'));
    assert!(!state.footer_hints_visible, "'?' hides them again");
    // Clicking [Save] saves WITHOUT exiting; [Save & Exit] submits.
    let mut tui = test_tui();
    tui.state = state;
    let size = tui.terminal.size().unwrap();
    let map = form_layout(size.into(), &tui.state);
    let (_, save) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::Save)
        .unwrap();
    assert_eq!(
        tui.handle_mouse(mouse_down(save.x + 1, save.y)).unwrap(),
        Some(FormAction::SaveOnly),
        "clicking [Save] saves without exiting"
    );
    let (_, exit) = map
        .buttons
        .iter()
        .find(|(b, _)| *b == ButtonId::SaveExit)
        .unwrap();
    assert_eq!(
        tui.handle_mouse(mouse_down(exit.x + 1, exit.y)).unwrap(),
        None,
        "Save & Exit waits for release so teardown cannot leak mouse CSI"
    );
    assert_eq!(
        tui.handle_mouse(mouse_up(exit.x + 1, exit.y)).unwrap(),
        Some(FormAction::Submit),
        "releasing on [Save & Exit] submits"
    );
}

// ── DUT panel labels ─────────────────────────────────

/// dut name / dev_ctrl (no separate type row) / login pass / (ms)
/// units / supported controller types hint.
#[test]
fn test_dut_panel_labels_and_supported_types() {
    let mut state = create_state(&InitOptions::default());
    state.toggle_other(); // collapse advanced → taller DUT box
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    ); // dev_ctrl ON (backend selected — the relay fields show directly)
    // 120x64: the two-column DUT content (the LEFT/RIGHT box split)
    // fits comfortably with the relay section ON.
    let map = form_layout(Rect::new(0, 0, 120, 64), &state);
    assert!(
        !map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayType(0, 0)),
        "the separate type row is gone"
    );
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutAdvanced(0, 0, "target.login_pass")),
        "login pass in the DUT content box"
    );
    let terminal = terminal_with(&mut state, 120, 64);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(joined.contains("dut name"), "{joined}");
    assert!(joined.contains("dev_ctrl"), "{joined}");
    assert!(joined.contains("login password"), "{joined}");
    assert!(joined.contains("reset time (ms)"), "{joined}");
    assert!(joined.contains("power-off time (ms)"), "{joined}");
    assert!(joined.contains("── relay ─"), "{joined}"); // box title row
    // The "supported:" hint is gone; the options live in the
    // controller picker popup instead.
    assert!(!joined.contains("supported:"), "{joined}");
    assert!(joined.contains("ch340"), "{joined}");
}

/// `first_reference_lines`: N meaningful lines, CR/blank stripped —
/// the exact window the relay cycle saves as the similarity seed.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_first_reference_lines_window() {
    let raw = b"\r\nser2net port tcp\r\nU-Boot 2023.10\r\n\r\nDRAM: 2 GiB\r\n";
    let lines = first_reference_lines(raw, 50);
    assert_eq!(lines, ["ser2net port tcp", "U-Boot 2023.10", "DRAM: 2 GiB"]);
    assert_eq!(first_reference_lines(raw, 2).len(), 2, "bounded window");
}

/// RFC 2217 SET_BAUDRATE acknowledgement parsing (big-endian rate,
/// terminated by IAC SE).
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_rfc2217_baud_ack_parsing() {
    let ack = [0xff, 0xfa, 44, 101, 0x00, 0x16, 0xe3, 0x60, 0xff, 0xf0];
    assert_eq!(rfc2217_baud_ack(&ack), Some(1_500_000));
    // Buried in negotiation traffic.
    let noisy = [0xffu8, 0xfb, 0x2c]
        .iter()
        .copied()
        .chain(ack)
        .collect::<Vec<_>>();
    assert_eq!(rfc2217_baud_ack(&noisy), Some(1_500_000));
    assert_eq!(rfc2217_baud_ack(&[0xff, 0xfb, 0x2c]), None);
    assert_eq!(rfc2217_baud_ack(b""), None);
}

/// The per-channel relay liveness probe: a fake ch340 relay answers
/// the STATUS packet → Ok; a silent listener → Err.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_relay_channel_status_probe() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = l.accept().unwrap();
        let mut b = [0u8; 4];
        sock.read_exact(&mut b).unwrap();
        // Echo a status reply (any bytes count as a live relay).
        sock.write_all(&[0xa0, b[1], 0x05, 0x00]).unwrap();
    });
    assert_eq!(
        probe_relay_channel_status("127.0.0.1", port, 1),
        ProbeMark::Ok
    );
    // Nothing listening → Err.
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    assert_eq!(
        probe_relay_channel_status("127.0.0.1", dead_port, 1),
        ProbeMark::Err
    );
}

/// dev_ctrl = ch340-relay makes the relay port and the reset test
/// MANDATORY: a missing port fails both rows (✗), never skips
/// (user rule).
#[test]
fn test_ch340_backend_makes_relay_port_and_reset_mandatory() {
    let mut tui = test_tui();
    let test_dir = tempfile::TempDir::new().unwrap();
    tui.write_ctx = Some((
        PreparedInit {
            target_path: test_dir.path().join(".target.jsonc"),
            existing_text: None,
            existing: None,
            schema: tui.state.schema.clone(),
            created: true,
        },
        quick(),
    ));
    // Prerequisites to reach the DUT probe block at all.
    tui.state.set_value(FieldKey::HostIp(0), "127.0.0.1".into());
    tui.state.set_value(FieldKey::DutPort(0, 0), "41002".into());
    let fk_ctrl = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    tui.state.set_value(fk_ctrl, "ch340-relay".into());
    // relay port left EMPTY.
    tui.start_test_run();
    assert!(
        tui.state.probes.contains_key(&FieldKey::DutRelayPort(0, 0)),
        "the relay PORT row is armed even with an empty port"
    );
    assert!(
        tui.state
            .probes
            .contains_key(&FieldKey::DutRelayReset(0, 0)),
        "the RESET row is armed even with an empty port"
    );
    // Drain the worker: both marks must be Err (loud ✗).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        tui.poll_test_run();
        if tui.state.test_run.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        tui.state.probes.get(&FieldKey::DutRelayPort(0, 0)),
        Some(&ProbeMark::Err),
        "empty port under ch340-relay fails loudly"
    );
    assert_eq!(
        tui.state.probes.get(&FieldKey::DutRelayReset(0, 0)),
        Some(&ProbeMark::Err),
        "reset row fails loudly without a relay port"
    );
}

#[test]
fn test_serial_port_is_mandatory_and_none_ctrl_skips_relay_tests() {
    let mut tui = test_tui();
    let test_dir = tempfile::TempDir::new().unwrap();
    tui.write_ctx = Some((
        PreparedInit {
            target_path: test_dir.path().join(".target.jsonc"),
            existing_text: None,
            existing: None,
            schema: tui.state.schema.clone(),
            created: true,
        },
        quick(),
    ));
    tui.state.set_value(FieldKey::HostIp(0), "127.0.0.1".into());
    tui.state.set_value(FieldKey::DutPort(0, 0), "41002".into());
    tui.state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    tui.state.set_value(
        FieldKey::DutAdvanced(0, 0, "flash.list_devices_cmd"),
        "ld".into(),
    );
    tui.state.set_value(
        FieldKey::DutAdvanced(0, 0, "flash.loader_cmd"),
        "rockusb 0 mmc 0".into(),
    );
    tui.start_test_run();

    assert!(
        tui.state.probes.contains_key(&FieldKey::DutPort(0, 0)),
        "the MCP baud/transport check must always mark serial.port"
    );
    for skipped in [FieldKey::DutRelayPort(0, 0), FieldKey::DutRelayReset(0, 0)] {
        assert!(
            !tui.state.probes.contains_key(&skipped),
            "dev_ctrl=none must skip relay-dependent probe {skipped:?}"
        );
    }
    assert!(
        tui.state
            .probes
            .contains_key(&FieldKey::DutAdvanced(0, 0, "uboot.interrupt_char")),
        "dev_ctrl=none still tests the interrupt char through soft reboot"
    );
    let reference_key = FieldKey::DutAdvanced(0, 0, "monitor.reference_log");
    assert!(
        tui.state.probes.contains_key(&reference_key),
        "dev_ctrl=none must visibly block reference_log"
    );
    assert!(
        tui.state
            .probe_details
            .get(&reference_key)
            .is_some_and(|detail| detail.contains("requires an enabled dev_ctrl")),
        "the blocked row must explain its dev_ctrl dependency"
    );
    assert!(
        tui.state.probes.contains_key(&flash::devices_key(0, 0)),
        "dev_ctrl=none must retain the MCP-serial loader_cmd probe"
    );

    let mut no_trigger = test_tui();
    let no_trigger_dir = tempfile::TempDir::new().unwrap();
    no_trigger.write_ctx = Some((
        PreparedInit {
            target_path: no_trigger_dir.path().join(".target.jsonc"),
            existing_text: None,
            existing: None,
            schema: no_trigger.state.schema.clone(),
            created: true,
        },
        quick(),
    ));
    no_trigger
        .state
        .set_value(FieldKey::HostIp(0), "127.0.0.1".into());
    no_trigger
        .state
        .set_value(FieldKey::DutPort(0, 0), "41002".into());
    no_trigger.state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    no_trigger.state.set_value(
        FieldKey::DutAdvanced(0, 0, "flash.list_devices_cmd"),
        "ld".into(),
    );
    no_trigger.start_test_run();
    assert!(
        !no_trigger
            .state
            .probes
            .contains_key(&flash::devices_key(0, 0)),
        "without loader_cmd/recovery/maskrom there is no flash probe"
    );
}

/// Every DUT in one project uses the same project MCP port. Explicit
/// overrides remain sorted and de-duplicated.
#[test]
fn test_mcp_ports_for_dut_uses_project_singleton_port() {
    let cwd = std::path::Path::new("/tmp/example-project");
    let per_dut = sermcp::ports::project_dut_mcp_port(cwd, "board1");
    let project = sermcp::ports::project_mcp_port(cwd);
    let ports = mcp_ports_for_dut(cwd, "board1");
    assert_eq!(per_dut, project, "DUT entries share the project MCP port");
    assert!(ports.contains(&project), "project port present");
    let mut sorted = ports.clone();
    sorted.sort_unstable();
    assert_eq!(ports, sorted, "ports are sorted");
    sorted.dedup();
    assert_eq!(ports, sorted, "ports are de-duplicated");
}

/// With dev_ctrl = ch340-relay and a valid relay port, the DUT tests are
/// armed through the MCP-first probes: the reference capture lands on
/// monitor.reference_log and U-Boot entry on uboot.interrupt_char — reset
/// ch keeps only its channel-liveness mark, so it can never flash ✓→⚠.
#[test]
fn test_mcp_routing_arms_reference_and_uboot_rows() {
    let mut tui = test_tui();
    let test_dir = tempfile::TempDir::new().unwrap();
    tui.write_ctx = Some((
        PreparedInit {
            target_path: test_dir.path().join(".target.jsonc"),
            existing_text: None,
            existing: None,
            schema: tui.state.schema.clone(),
            created: true,
        },
        quick(),
    ));
    tui.state.set_value(FieldKey::HostIp(0), "127.0.0.1".into());
    tui.state.set_value(FieldKey::DutPort(0, 0), "41002".into());
    tui.state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    tui.state
        .set_value(FieldKey::DutRelayPort(0, 0), "41001".into());
    tui.state
        .set_value(FieldKey::DutRelayReset(0, 0), "1".into());
    tui.start_test_run();
    assert!(
        tui.state
            .probes
            .contains_key(&FieldKey::DutAdvanced(0, 0, "monitor.reference_log")),
        "reference capture lands on monitor.reference_log"
    );
    assert!(
        tui.state
            .probes
            .contains_key(&FieldKey::DutAdvanced(0, 0, "uboot.interrupt_char")),
        "U-Boot entry lands on uboot.interrupt_char"
    );
    assert!(
        tui.state
            .probes
            .contains_key(&FieldKey::DutRelayReset(0, 0)),
        "reset keeps its liveness mark"
    );

    // Load the UEFI case from .target.jsonc. The serial and relay ports
    // are ephemeral test endpoints, never board-specific constants.
    let temp = tempfile::TempDir::new().unwrap();
    let serial_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let serial_port = serial_listener.local_addr().unwrap().port();
    drop(serial_listener);
    let relay_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_port = relay_listener.local_addr().unwrap().port();
    drop(relay_listener);
    std::fs::write(
        temp.path().join(".target.jsonc"),
        format!(
            r#"{{
  "dev_hosts": [{{
    "ip": "127.0.0.1",
    "duts": [{{
      "dut_name": "uefi-dut",
      "serial": {{ "port": {serial_port} }},
      "loader": "uefi",
      "control": {{ "dev_ctrl": "ch340-relay" }},
      "relay": {{ "port": {relay_port}, "reset_ch": 1 }}
    }}]
  }}]
}}"#
        ),
    )
    .unwrap();
    let prepared = prepare_init(temp.path(), &quick()).unwrap();
    let uefi_state = FormState::new(prepared.schema, prepared.target_path.display().to_string());
    assert_eq!(
        uefi_state.values.get(&FieldKey::DutPort(0, 0)),
        Some(&serial_port.to_string()),
        "serial.port must come from .target.jsonc"
    );
    let mut uefi = FormTui::new(
        Terminal::new(TestBackend::new(80, 24)).unwrap(),
        KeyQueue::new(Vec::new()),
        uefi_state,
    );
    uefi.start_test_run();
    assert!(
        !uefi
            .state
            .probes
            .contains_key(&FieldKey::DutAdvanced(0, 0, "uboot.interrupt_char")),
        "loader=uefi must skip the U-Boot interrupt test"
    );
}

/// The strict prompt check: boot-log debris containing "=>" never
/// matches; only a trailing standalone "=>" line does.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_ends_at_uboot_prompt_strict() {
    assert!(ends_at_uboot_prompt(
        b"U-Boot 2023.10\r\nboard=>something\r\n=> "
    ));
    assert!(!ends_at_uboot_prompt(b"board=>something\r\nbooting..."));
    assert!(!ends_at_uboot_prompt(
        b"=> in the middle\r\nmore output\r\n"
    ));
    assert!(!ends_at_uboot_prompt(b""));
}

/// Interrupt-char parsing mirrors Config::uboot_interrupt_char.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_parse_interrupt_char() {
    assert_eq!(parse_interrupt_char("ctrl+c"), 0x03);
    assert_eq!(parse_interrupt_char("ctrl_c"), 0x03);
    assert_eq!(parse_interrupt_char("0x03"), 0x03);
    assert_eq!(parse_interrupt_char("2"), b'2');
    assert_ne!(parse_interrupt_char("2"), 0x02);
    assert_eq!(parse_interrupt_char("c"), b'c');
    assert_eq!(parse_interrupt_char("0x1b"), 0x1b);
    assert_eq!(parse_interrupt_char("junk"), 0x03);
}

#[test]
fn test_software_uboot_probe_passes_ctrl_c_from_the_form() {
    let args = uboot_probe_arguments("ctrl_c", true);
    assert_eq!(args["restart"], "software");
    assert_eq!(args["interrupt_char"], "ctrl_c");
    assert_eq!(args["failure_retry"], 1);
    assert_eq!(args["flood_interval_ms"], 100);
}

#[test]
fn test_hardware_uboot_probe_passes_interrupt_char_from_the_form() {
    let args = uboot_probe_arguments("0x03", false);
    assert_eq!(args["restart"], "hardware");
    assert_eq!(args["interrupt_char"], "0x03");
    assert_eq!(args["flood_duration_secs"], 10.0);
}

/// U-Boot interrupt probe against local fakes: the fake serial emits
/// an autoboot countdown after release and stops at `=>` ONLY when it
/// receives the interrupt char — proving the flood works end-to-end.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_uboot_interrupt_probe_stops_at_prompt() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let relay_l = TcpListener::bind("127.0.0.1:0").unwrap();
    let serial_l = TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_port = relay_l.local_addr().unwrap().port();
    let serial_port = serial_l.local_addr().unwrap().port();
    let released = Arc::new(AtomicBool::new(false));

    let rel = released.clone();
    std::thread::spawn(move || {
        let (mut sock, _) = relay_l.accept().unwrap();
        let mut b = [0u8; 4];
        while let Ok(n) = sock.read(&mut b) {
            if n == 0 {
                break;
            }
            if b[2] == sermcp::power_control::RELAY_OP_OFF {
                rel.store(true, Ordering::SeqCst);
            }
        }
    });
    std::thread::spawn(move || {
        let (mut sock, _) = serial_l.accept().unwrap();
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(50)));
        let mut sink = [0u8; 256];
        while !released.load(Ordering::SeqCst) {
            let _ = sock.read(&mut sink);
        }
        // Boot output INCLUDING debris that merely contains "=>" —
        // the loose old check false-passed on this (field-observed: a
        // fake test).
        sock.write_all(b"U-Boot 2023.10\r\nboard=>something debris\r\n")
            .unwrap();
        sock.write_all(b"Hit any key to stop autoboot: 3 2 1\r\n")
            .unwrap();
        // ...and stop ONLY on the interrupt char (0x03).
        let mut stopped = false;
        loop {
            match sock.read(&mut sink) {
                Ok(n) if n > 0 => {
                    if !stopped && sink[..n].contains(&0x03) {
                        stopped = true;
                        sock.write_all(b"\r\n=> ").unwrap();
                    } else if stopped {
                        // Interactive proof: a command gets a REAL
                        // U-Boot reply + a fresh prompt.
                        sock.write_all(b"\r\nU-Boot 2023.10 (Jan 01 2025)\r\n=> ")
                            .unwrap();
                    }
                }
                _ => {}
            }
        }
    });

    let mark = probe_uboot_interrupt(
        "127.0.0.1",
        relay_port,
        1,
        "127.0.0.1",
        serial_port,
        0x03,
        50,
    );
    assert_eq!(
        mark,
        ProbeMark::Ok,
        "flood stopped autoboot AND the interactive version reply proved the command line"
    );
}

/// When the relay reset channel is configured, the EMPTY
/// monitor.reference_log row auto-fills with the per-DUT default
/// path (requested); a typed path is never overwritten.
#[test]
fn test_reference_log_autofills_when_relay_reset_configured() {
    let mut state = create_state(&quick());
    let ctrl = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    let rport = FieldKey::DutRelayPort(0, 0);
    let reset = FieldKey::DutRelayReset(0, 0);
    let refk = FieldKey::DutAdvanced(0, 0, "monitor.reference_log");
    // The RELAY PORT alone (no reset channel) surfaces the path
    // (requested).
    state.set_value(rport.clone(), "41000".into());
    assert_eq!(
        state.values.get(&refk).map(String::as_str),
        Some(".dut-serial/dut1/reference-boot.log"),
        "the default reference path is surfaced from the relay port"
    );
    // A typed path wins.
    state.set_value(refk.clone(), "my-ref.log".into());
    state.set_value(reset.clone(), "2".into());
    assert_eq!(
        state.values.get(&refk).map(String::as_str),
        Some("my-ref.log"),
        "typed path never overwritten"
    );
    // dev_ctrl = none disables the autofill.
    state.values.remove(&refk);
    state.set_value(ctrl, "none".into());
    assert_eq!(
        state.values.get(&refk),
        None,
        "none disables the reference autofill"
    );
    let _ = rport;
}

/// The monitor.reference_log row renders its path GRAY (dim OVERLAY0)
/// until the TEST's capture probe passed, then in the NORMAL style
/// (requested): the gray/bright flip follows the probe mark, not
/// the file on disk. The text is ALWAYS visible — only its color
/// changes.
#[test]
fn test_reference_log_grays_until_test_passes() {
    fn path_fg(buffer: &ratatui::buffer::Buffer, path: &str) -> Option<ratatui::style::Color> {
        let chars: Vec<char> = path.chars().collect();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let hit = chars.iter().enumerate().all(|(i, c)| {
                    buffer
                        .cell((x + i as u16, y))
                        .is_some_and(|cell| cell.symbol() == c.to_string())
                });
                if hit {
                    return buffer.cell((x, y)).map(|c| c.fg);
                }
            }
        }
        None
    }
    let mut state = create_state(&quick());
    let refk = FieldKey::DutAdvanced(0, 0, "monitor.reference_log");
    let path = ".dut-serial/dut1/reference-boot.log";
    state.set_value(refk.clone(), path.into());
    // No TEST yet: the path renders GRAY.
    let terminal = terminal_with(&mut state, 160, 64);
    assert_eq!(
        path_fg(terminal.backend().buffer(), path),
        Some(OVERLAY0),
        "unproven path renders gray"
    );
    // A FAILED capture keeps it gray.
    state.probes.insert(refk.clone(), ProbeMark::Err);
    let terminal = terminal_with(&mut state, 160, 64);
    assert_eq!(
        path_fg(terminal.backend().buffer(), path),
        Some(OVERLAY0),
        "failed capture keeps the path gray"
    );
    // The capture probe PASSED: the path renders in the normal style.
    state.probes.insert(refk.clone(), ProbeMark::Ok);
    let terminal = terminal_with(&mut state, 160, 64);
    assert_eq!(
        path_fg(terminal.backend().buffer(), path),
        Some(ratatui::style::Color::Reset),
        "passed capture renders the path normally"
    );
}

/// Real boots vary in volatile values (DDR training results, counts)
/// but keep identical structure — the masked stability gate passes
/// them (field-observed: unmasked never reached 0.93).
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_reference_stability_masks_volatile_values() {
    let a = "DDR 2f85f4b2d4 cym 25/07/17-09:48.04,fwver: v1.09\nch0 ttot10\nch1 ttot16\nIn\nlpddr5 nt 512\n";
    let b = "DDR 9a12cc77e1 cym 25/07/17-11:02.31,fwver: v1.09\nch0 ttot12\nch1 ttot16\nIn\nlpddr5 nt 512\n";
    let c = "DDR 4477ab9055 cym 25/07/17-14:22.59,fwver: v1.09\nch0 ttot10\nch1 ttot18\nIn\nlpddr5 nt 512\n";
    assert!(
        reference_is_stable(&[a.into(), b.into(), c.into()]),
        "structure-identical boots with volatile values are stable"
    );
    let d = "U-Boot 2023.10\nDRAM: 2 GiB\nKernel cmdline boot=mmc\n";
    assert!(
        !reference_is_stable(&[a.into(), d.into()]),
        "genuinely different outputs are NOT stable"
    );
}

#[test]
fn test_ssh_host_key_change_detection_is_narrow() {
    assert!(ssh_host_key_changed(
        b"WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n"
    ));
    assert!(ssh_host_key_changed(
        b"Offending ED25519 key in /home/user/.ssh/known_hosts:4\n"
    ));
    assert!(!ssh_host_key_changed(b"Permission denied (publickey).\n"));
    assert!(!ssh_host_key_changed(
        b"ssh: connect to host host.invalid port 22: Connection refused\n"
    ));
    assert!(!ssh_host_key_changed(b"Host key verification failed.\n"));
}

/// The press-window noise judge ignores the ser2net connect banner
/// and telnet IAC handshake; real board output counts as noise.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_meaningful_serial_noise_ignores_banner_and_iac() {
    // Real capture shape: IAC negotiations + banner, nothing else.
    let connect_only =
        b"\xff\xfb\x03\xff\xfd\x03\r\nser2net port telnet(rfc2217),tcp,41002 device\r\n\r\n";
    assert!(
        !meaningful_serial_noise(connect_only),
        "banner is not noise"
    );
    // Board output while pressed IS noise (wrong channel / no reset).
    assert!(
        meaningful_serial_noise(b"\r\nU-Boot 2023.10\r\n"),
        "board talk is noise"
    );
    assert!(!meaningful_serial_noise(b""), "silence is silence");
}

/// Full relay reset cycle against local fakes: a fake relay (4-byte
/// ch340 protocol) whose OFF triggers a fake serial to emit boot
/// lines — press must observe silence, release must capture, and the
/// first REFERENCE_LINES lines must land in the save file.
#[cfg(feature = "direct-dut-probe")]
#[test]
fn test_relay_reset_cycle_saves_reference_log() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    let relay_l = TcpListener::bind("127.0.0.1:0").unwrap();
    let serial_l = TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_port = relay_l.local_addr().unwrap().port();
    let serial_port = serial_l.local_addr().unwrap().port();
    // Fake relay: every OP_OFF bumps the release counter.
    let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rel = releases.clone();
    std::thread::spawn(move || {
        let (mut sock, _) = relay_l.accept().unwrap();
        let mut b = [0u8; 4];
        while let Ok(n) = sock.read(&mut b) {
            if n == 0 {
                break;
            }
            if b[2] == sermcp::power_control::RELAY_OP_OFF {
                rel.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    // Fake serial: silent while pressed; on EVERY release emit the
    // same deterministic boot burst (3 cycles in the probe).
    let rel = releases.clone();
    std::thread::spawn(move || {
        let (mut sock, _) = serial_l.accept().unwrap();
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(50)));
        let mut sink = [0u8; 1024];
        let mut seen = 0usize;
        loop {
            let cur = rel.load(Ordering::SeqCst);
            if cur > seen {
                seen = cur;
                let mut boot = String::new();
                // A stale leftover LINE first — the probe's
                // alignment step must drop it before the boot window.
                boot.push_str("stale-leftover-line\r\n");
                for i in 0..80 {
                    boot.push_str(&format!("boot line {i:03}\r\n"));
                }
                if sock.write_all(boot.as_bytes()).is_err() {
                    break;
                }
            }
            let _ = sock.read(&mut sink);
        }
    });

    let tmp = tempfile::TempDir::new().unwrap();
    let save = tmp.path().join("dut-x/reference-boot.log");
    let mark = probe_relay_reset_cycle(
        "127.0.0.1",
        relay_port,
        1,
        "127.0.0.1",
        serial_port,
        &save,
        50,
    );
    assert_eq!(mark, ProbeMark::Ok, "press silent + release captured");
    let saved = std::fs::read_to_string(&save).unwrap();
    let count = saved.lines().count();
    assert_eq!(count, REFERENCE_LINES, "exactly the reference window saved");
    assert!(saved.starts_with("boot line 000"), "first boot line first");
}

/// User-visible blank under none: with dev_ctrl = none
/// the rendered buffer shows NO relay sub-row labels below dev_ctrl —
/// the reserved slots stay empty (while the box height is unchanged).
#[test]
fn test_none_backend_renders_blank_below_dev_ctrl() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    let terminal = terminal_with(&mut state, 120, 64);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(joined.contains("dev_ctrl"), "{joined}");
    for label in [
        "reset",
        "maskrom",
        "recovery",
        "power",
        "reset time (ms)",
        "power-off (ms)",
    ] {
        assert!(
            !joined.contains(label),
            "{label} must NOT render under none: {joined}"
        );
    }
}

/// The controller dropdown opens DIRECTLY BELOW its Select row (the
/// \u{25be} spot), not as a centered modal (deliberate).
#[test]
fn test_picker_dropdown_anchors_below_select_row() {
    let mut state = create_state(&quick());
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    state.set_focus(FocusSlot::Field(fk.clone()));
    handle_form_key(&mut state, chr(' ')); // opens the picker
    assert!(state.picker.is_some());
    let terminal = terminal_with(&mut state, 120, 40);
    let buffer = terminal.backend().buffer();
    // The dropdown's border top row must sit one row below the
    // dev_ctrl row. Find the dev_ctrl label row y in the layout...
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let (_, row_rect) = map.fields.iter().find(|(k, _)| *k == fk).unwrap();
    // ...and find the dropdown: the row below must contain the first
    // option text (ch340-relay) as a bordered list entry.
    // y+1 is the dropdown's top BORDER; the first option sits inside
    // it (y+2). Assert both: the border opens right below the row and
    // the option list is anchored at the LABEL's left edge.
    let border: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, row_rect.y + 1))
                .map(|c| c.symbol())
                .unwrap_or(" ")
                .to_string()
        })
        .collect();
    assert!(
        border.contains('\u{250c}'),
        "the dropdown border opens directly below the dev_ctrl row: {border:?}"
    );
    let map2 = form_layout(Rect::new(0, 0, 120, 40), &state);
    let label_x = map2
        .dut_rows
        .iter()
        .find(|r| r.key.as_ref() == Some(&fk))
        .map(|r| r.label_rect.x)
        .unwrap();
    let opt_y = row_rect.y + 2;
    let opt: String = (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, opt_y))
                .map(|c| c.symbol())
                .unwrap_or(" ")
                .to_string()
        })
        .collect();
    assert!(opt.contains("ch340-relay"), "first option renders: {opt:?}");
    assert!(
        opt.find("ch340-relay")
            .is_some_and(|pos| pos >= label_x as usize),
        "the option list aligns with the label column (label_x={label_x}): {opt:?}"
    );
    // Excel-style: the CURSOR row is a full-width
    // selection bar (accent background across the row) and the
    // CURRENT value carries a leading check mark.
    let bar_cells_have_bg = (0..buffer.area.width)
        .filter(|&x| {
            buffer
                .cell((x, opt_y))
                .is_some_and(|c| c.bg != ratatui::style::Color::Reset)
        })
        .count();
    assert!(
        bar_cells_have_bg >= 10,
        "the cursor row is a full-row selection bar ({bar_cells_have_bg} bg cells)"
    );
}

/// ComboBox dropdown: the ✓ rides the COMMITTED value ("none") and
/// the browse cursor starts there (blue highlight); the bottom
/// shortcut bar is GONE (— users naturally try ↑↓/Enter/
/// Esc, no hint needed).
#[test]
fn test_picker_combobox_check_and_no_hint_bar() {
    let mut state = create_state(&quick());
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    state.set_value(fk.clone(), "none".into()); // committed = none
    state.set_focus(FocusSlot::Field(fk.clone()));
    handle_form_key(&mut state, chr(' ')); // opens the picker
    let terminal = terminal_with(&mut state, 120, 40);
    let buffer = terminal.backend().buffer();
    let text: String = (0..buffer.area.height)
        .flat_map(|y| {
            (0..buffer.area.width).map(move |x| {
                buffer
                    .cell((x, y))
                    .map(|c| c.symbol())
                    .unwrap_or(" ")
                    .to_string()
            })
        })
        .collect();
    assert!(
        text.contains('\u{2713}'),
        "current value carries a check mark"
    );
    assert!(
        !text.contains("↑↓"),
        "the bottom shortcut bar is removed: {text}"
    );
}

/// The loader picker renders options VERBATIM (like dev_ctrl): the
/// dropdown shows "uboot" and "uefi"; the "not implemented" note
/// lives BELOW the field, not in the list.
#[test]
fn test_loader_picker_renders_options_verbatim() {
    let mut state = create_state(&quick());
    let fk = FieldKey::DutAdvanced(0, 0, "loader");
    state.set_focus(FocusSlot::Field(fk.clone()));
    handle_form_key(&mut state, chr(' ')); // opens the picker
    let terminal = terminal_with(&mut state, 120, 40);
    let buffer = terminal.backend().buffer();
    let text: String = (0..buffer.area.height)
        .flat_map(|y| {
            (0..buffer.area.width).map(move |x| {
                buffer
                    .cell((x, y))
                    .map(|c| c.symbol())
                    .unwrap_or(" ")
                    .to_string()
            })
        })
        .collect();
    assert!(text.contains("uboot"), "uboot renders verbatim: {text}");
    assert!(text.contains("uefi"), "uefi renders verbatim: {text}");
}

// ── Required-field marker ───────────────────────────

/// HostIp and DutPort are the ONLY blocking required fields; their
/// labels carry an amber `*` (error-red when the field errors);
/// optional fields carry none.
#[test]
fn test_required_fields_asterisk_marker() {
    let mut state = create_state(&InitOptions::default());
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let required = |k: &FieldKey| {
        map.host_rows
            .iter()
            .chain(map.dut_rows.iter())
            .find(|r| r.key.as_ref() == Some(k))
            .map(|r| r.required)
            .unwrap_or(false)
    };
    assert!(required(&FieldKey::HostIp(0)), "ip is required");
    assert!(
        required(&FieldKey::DutPort(0, 0)),
        "serial.port is required"
    );
    assert!(!required(&FieldKey::DutLoginUser(0, 0)));
    assert!(
        !required(&FieldKey::HostName(0)),
        "host_name is optional — never required"
    );
    // The amber '*' paints right after the label.
    let terminal = terminal_with(&mut state, 120, 40);
    let buf = terminal.backend().buffer();
    let ip_row = map
        .host_rows
        .iter()
        .find(|r| r.key.as_ref() == Some(&FieldKey::HostIp(0)))
        .unwrap();
    let star = &buf.content[buf.index_of(
        ip_row.label_rect.x + ip_row.label.chars().count() as u16 + 1,
        ip_row.label_rect.y,
    )];
    assert_eq!(star.symbol(), "*", "required asterisk painted");
    assert_eq!(star.fg, REQUIRED_MARK, "amber marker");
    // Error precedence: the asterisk turns red with the label.
    state
        .errors
        .insert(FieldKey::HostIp(0), "ip is required".into());
    let terminal = terminal_with(&mut state, 120, 40);
    let buf = terminal.backend().buffer();
    let star = &buf.content[buf.index_of(
        ip_row.label_rect.x + ip_row.label.chars().count() as u16 + 1,
        ip_row.label_rect.y,
    )];
    assert_eq!(star.fg, Color::Red, "error-red precedence");
}

// ── Save-only semantics ──────────────────────────────

/// [Save] writes the config and KEEPS the loop running: the file lands
/// mid-loop, and a later Esc still aborts the session.
#[test]
fn test_save_button_writes_without_exiting() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let mut state = FormState::new(
        prepared.schema.clone(),
        prepared.target_path.display().to_string(),
    );
    type_into(&mut state, FieldKey::HostIp(0), "10.0.0.1");
    type_into(&mut state, FieldKey::DutPort(0, 0), "41002");
    state.focus = FocusSlot::Button(ButtonId::Save);
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(100, 30)).unwrap(),
        KeyQueue::new(vec![ev(enter()), ev(esc())]),
        state,
    );
    tui.write_ctx = Some((prepared.clone(), quick()));
    // Enter on [Save] → SaveOnly: the write lands, the loop keeps
    // running, then Esc aborts — the file must already exist.
    let res = tui.form_loop();
    assert!(res.is_err(), "Esc aborts after the in-loop save");
    assert!(
        tmp.path().join(".target.jsonc").exists(),
        "file written mid-loop"
    );
    match &tui.state.note {
        Some((NoteKind::Info, msg)) => assert!(msg.starts_with("saved —"), "{msg}"),
        other => panic!("expected a saved note, got {other:?}"),
    }
}

// ── Layout dynamics ──────────────────────────────────

/// Dragging the divider pins the host height (clamped to keep a
/// minimal DUT panel); Up ends the drag.
#[test]
fn test_divider_drag_pins_and_clamps() {
    // 120x40 (was 80x24): at 80x24 the cramped floor reserves the
    // whole body for the host panel (DUT content 18+ rows), so ANY
    // drag target already exceeds the clamp — the unclamped pin path
    // was unreachable there.
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&quick()),
    );
    let area = tui.terminal.size().unwrap();
    let map = form_layout(Rect::new(0, 0, area.width, area.height), &tui.state);
    // Down on the divider starts the drag.
    tui.handle_mouse(mouse_down(map.divider.x + 5, map.divider.y))
        .unwrap();
    assert!(
        tui.state.divider_drag,
        "Down on the divider starts the drag"
    );
    // Drag to a row below pins the height.
    let target = map.divider.y.saturating_add(6);
    tui.handle_mouse(mouse_drag(map.divider.x + 5, target))
        .unwrap();
    assert_eq!(
        tui.state.host_h_pin,
        Some(target.saturating_sub(map.host_panel.y)),
        "drag pins the host height"
    );
    // Up ends the drag.
    tui.handle_mouse(mouse_up(map.divider.x + 5, target))
        .unwrap();
    assert!(!tui.state.divider_drag);
    // The clamp: dragging far below caps the pin so the DUT keeps its
    // minimal height (the DUT_MIN_ABS reserve — the DUT box keeps ONE
    // interior title row at the floor).
    tui.state.host_h_pin = None;
    tui.handle_mouse(mouse_down(map.divider.x + 5, map.divider.y))
        .unwrap();
    let far = map.divider.y.saturating_add(90);
    tui.handle_mouse(mouse_drag(map.divider.x + 5, far))
        .unwrap();
    let body = map.dut_panel.bottom().saturating_sub(map.host_panel.y);
    assert_eq!(
        tui.state.host_h_pin,
        Some(body.saturating_sub(1).saturating_sub(DUT_MIN_ABS)),
        "pin clamps to keep a minimal DUT panel"
    );
}

/// The scrollbar thumb: present only on overflow, 1-cell wide, inside
/// the container, height ≥ 1.
#[test]
fn test_scrollbar_thumb_math() {
    let inner = Rect::new(0, 0, 10, 10);
    // No overflow → no thumb.
    assert!(scrollbar_thumb(inner, 5, 0).is_none());
    // Overflow → a thumb on the right edge, inside the container.
    let thumb = scrollbar_thumb(inner, 30, 0).expect("overflow shows a thumb");
    assert_eq!(thumb.x, inner.right());
    assert_eq!(thumb.width, 1);
    assert!(thumb.height >= 1);
    assert!(thumb.y >= inner.y && thumb.bottom() <= inner.bottom());
    // Scrolled to the end → the thumb bottoms out inside the track.
    let end = scrollbar_thumb(inner, 30, 20).unwrap();
    assert!(end.y <= inner.bottom().saturating_sub(1));
    assert!(end.y >= thumb.y, "the thumb moves down with the scroll");
}

/// The pin clears on structure changes (toggle/add host/DUT).
#[test]
fn test_pin_clears_on_structure_changes() {
    let mut state = create_state(&quick());
    state.host_h_pin = Some(10);
    state.toggle_other();
    assert_eq!(state.host_h_pin, None, "toggle clears the pin");
    state.host_h_pin = Some(10);
    state.add_host();
    assert_eq!(state.host_h_pin, None, "add_host clears the pin");
    state.host_h_pin = Some(10);
    state.add_dut(0);
    assert_eq!(state.host_h_pin, None, "add_dut clears the pin");
    state.host_h_pin = Some(10);
    state.remove_host(1);
    assert_eq!(state.host_h_pin, None, "remove_host clears the pin");
}

// ── TEST button ──────────────────────────────────────────────────
/// A TEST on an empty CREATE is a dry-run: errors surface + focus
/// jumps, NOTHING is written.
#[test]
fn test_test_button_dry_run_never_writes() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(100, 30)).unwrap(),
        KeyQueue::new(Vec::new()),
        FormState::new(
            prepared.schema.clone(),
            prepared.target_path.display().to_string(),
        ),
    );
    tui.write_ctx = Some((prepared, quick()));
    // Test on the empty form: Phase A errors, nothing probed/written.
    let res = tui.apply_action(FormAction::Test).unwrap();
    assert!(res.is_none());
    assert!(!tui.state.errors.is_empty(), "errors surfaced");
    assert!(tui.state.test_run.is_none(), "no probes on an invalid form");
    assert!(
        !tmp.path().join(".target.jsonc").exists(),
        "TEST never writes"
    );
}

#[test]
fn test_temporary_mcp_cleanup_reaps_child_and_restores_inventory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dut_dir = tmp.path().join(".dut-serial/dut1");
    std::fs::create_dir_all(&dut_dir).unwrap();
    let inventory = tmp.path().join(".dut-serial/inventory.json");
    let original = br#"{"schema_version":1,"written_by":"dutabo","mcp_pid":null}"#;
    std::fs::write(&inventory, original).unwrap();
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    std::fs::write(dut_dir.join("mcp.pid"), pid.to_string()).unwrap();
    std::fs::write(
        &inventory,
        format!(r#"{{"schema_version":1,"written_by":"mcp-server","mcp_pid":{pid}}}"#),
    )
    .unwrap();

    let mut handle = McpHandle {
        children: vec![SpawnedMcp {
            child,
            project_dir: tmp.path().to_path_buf(),
            dut_name: "dut1".into(),
        }],
        inventory_snapshots: vec![InventorySnapshot {
            project_dir: tmp.path().to_path_buf(),
            content: Some(original.to_vec()),
        }],
        test_configs: Vec::new(),
        blocked_ports: std::collections::BTreeSet::new(),
    };
    handle.stop_spawned();

    assert!(
        !sermcp::lock_manager::process_alive(pid),
        "temporary MCP child is reaped"
    );
    assert!(
        !dut_dir.join("mcp.pid").exists(),
        "owned pid residue removed"
    );
    assert_eq!(std::fs::read(inventory).unwrap(), original);
    assert!(handle.children.is_empty());
    assert!(handle.inventory_snapshots.is_empty());
}

#[test]
fn test_health_response_matches_real_json_without_escape_bytes() {
    assert!(health_response_is_ready(
        b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"ok\"}"
    ));
    assert!(!health_response_is_ready(
        b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"starting\"}"
    ));
    assert!(!health_response_is_ready(br#"{\"status\":\"ok\"}"#));
}

#[test]
fn test_ensure_does_not_spawn_duplicate_for_owned_live_dut() {
    let temp = tempfile::TempDir::new().unwrap();
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut handle = McpHandle {
        children: vec![SpawnedMcp {
            child,
            project_dir: temp.path().to_path_buf(),
            dut_name: "dut1".into(),
        }],
        inventory_snapshots: Vec::new(),
        test_configs: Vec::new(),
        blocked_ports: std::collections::BTreeSet::new(),
    };

    assert!(
        !handle.ensure(temp.path(), "dut1", 3099, &temp.path().join("test.jsonc")),
        "a live child owned by this TEST must win before pid-file discovery"
    );
    assert_eq!(handle.children.len(), 1);
    handle.stop_spawned();
}

/// Editing invalidates the TEST marks.
#[test]
fn test_edits_clear_probe_marks() {
    let mut state = create_state(&quick());
    state.probes.insert(FieldKey::HostIp(0), ProbeMark::Ok);
    state.set_value(FieldKey::HostIp(0), "10.0.0.9".into());
    assert!(state.probes.is_empty(), "edits clear the marks");
}

/// The footer buttons are disjoint and ordered Test < Save < Save & Exit.
#[test]
fn test_footer_buttons_disjoint() {
    let state = create_state(&quick());
    let map = form_layout(Rect::new(0, 0, 120, 30), &state);
    let r = |b: ButtonId| {
        map.buttons
            .iter()
            .find(|(x, _)| *x == b)
            .map(|(_, rect)| *rect)
            .unwrap()
    };
    let test = r(ButtonId::Test);
    let save = r(ButtonId::Save);
    let exit = r(ButtonId::SaveExit);
    assert!(test.right() < save.x, "[Test] sits left of [Save]");
    assert!(save.right() < exit.x, "[Save] sits left of [Save & Exit]");
}

/// A bounded probe against a refused port resolves promptly with an
/// Err mark (the worker's timeouts keep the UI responsive).
#[test]
fn test_probe_worker_marks_refused_connect_as_err() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        run_probes(
            vec![(
                FieldKey::HostIp(0),
                ProbeTarget::TcpConnect {
                    host: "127.0.0.1".into(),
                    port: 1,
                },
            )],
            tx,
        )
    });
    // Skip the worker's "testing…" running events; the first RESOLVED
    // event is the verdict.
    let (key, mark, _detail) =
        std::iter::from_fn(|| rx.recv_timeout(std::time::Duration::from_secs(3)).ok())
            .find(|(_, mark, _)| *mark != ProbeMark::Pending)
            .expect("a resolved mark arrives");
    assert_eq!(key, FieldKey::HostIp(0));
    assert_eq!(mark, ProbeMark::Err, "refused port → Err");
}

// ── Select affordance ───────────────────────────────

/// Empty Select fields show the ▾ marker AND an ellipsis-fit options
/// hint — the dropdown is never invisible.
#[test]
fn test_empty_select_shows_dropdown_affordance() {
    let mut state = create_state(&InitOptions::default());
    let terminal = terminal_with(&mut state, 120, 40);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(
        joined.contains("dev_ctrl"),
        "the control.dev_ctrl row renders as 'dev_ctrl': {joined}"
    );
    assert!(joined.contains("▼"), "the dropdown marker renders");
    assert!(
        joined.contains("ch340") || joined.contains("…"),
        "the options hint renders (possibly ellipsis-fit): {joined}"
    );
}

// ── Select interaction ──────────────────────────────

/// Space opens the controller picker (— cycling was easy
/// to overshoot); Enter confirms the highlighted option. A bare space
/// never seeds into the buffer.
#[test]
fn test_space_cycles_select_and_never_seeds_space() {
    let mut state = create_state(&InitOptions::default());
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    state.set_focus(FocusSlot::Field(fk.clone()));
    handle_form_key(&mut state, chr(' '));
    assert!(
        state.picker.is_some(),
        "Space opens the controller picker modal"
    );
    handle_form_key(&mut state, key(KeyCode::Enter));
    assert_eq!(
        state.values.get(&fk).map(String::as_str),
        Some("ch340-relay"),
        "Enter confirms the highlighted option"
    );
    assert!(state.picker.is_none(), "Enter closes the picker");
    // Digits are still direct shortcuts over the replaceable options
    // (two options only).
    handle_form_key(&mut state, chr('2'));
    assert_eq!(
        state.values.get(&fk).map(String::as_str),
        Some("none"),
        "digit 2 picks the second option"
    );
}

// ── control.dev_ctrl association + mouse ────────────

/// Clicking a Select field cycles the next option (mouse-first).
#[test]
fn test_click_select_field_cycles() {
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&InitOptions::default()),
    );
    let size = tui.terminal.size().unwrap();
    let map = form_layout(size.into(), &tui.state);
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    let (_, rect) = map.fields.iter().find(|(k, _)| *k == fk).unwrap();
    tui.handle_mouse(mouse_down(rect.x + 1, rect.y)).unwrap();
    assert!(
        tui.state.picker.is_some(),
        "click opens the controller picker modal "
    );
}

/// control.dev_ctrl = none/disabled removes the WHOLE relay section
/// from the DUT panel — the relay BOX keeps only dev_ctrl itself (the
/// association).
#[test]
fn test_none_ctrl_hides_dut_dev_ctrl_section() {
    let mut state = create_state(&InitOptions::default());
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    state.set_value(fk.clone(), "ch340-relay".into());
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayPort(0, 0)),
        "dev_ctrl section visible with a backend"
    );
    state.set_value(fk, "none".into());
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    // Under none the sub-rows keep their SLOTS (constant height — no
    // jump) but render BLANK: nothing shows below dev_ctrl, and they
    // are unfocusable.
    assert!(
        map.fields
            .iter()
            .any(|(k, _)| *k == FieldKey::DutRelayPort(0, 0)),
        "the sub-row slots stay reserved under none (no jump)"
    );
    assert!(
        !state
            .dut_zone_fields()
            .contains(&FieldKey::DutRelayPort(0, 0)),
        "blank rows are not focusable"
    );
    // And no hidden row is focusable (never focus an invisible rect;
    // the gate row is gone entirely —).
    assert!(
        !state
            .dut_zone_fields()
            .contains(&FieldKey::DutRelayPort(0, 0))
    );
    assert!(
        !state
            .dut_zone_fields()
            .contains(&FieldKey::DutRelayGate(0, 0))
    );
}

/// dev_ctrl = none/disabled must also keep the relay SUB-fields out of
/// the zone walks even when a prefilled gate buffer still says yes
/// (an existing relay section + a disabled backend) — hidden rows must
/// never be focusable (the association, the whole dev_ctrl
/// section hides and "the gate is not focusable").
#[test]
fn test_none_ctrl_hides_relay_subfields_from_zone_walks() {
    let mut state = create_state(&InitOptions::default());
    state.values.insert(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    state
        .values
        .insert(FieldKey::DutRelayGate(0, 0), "yes".into());
    let zone = state.dut_zone_fields();
    assert!(
        !zone.contains(&FieldKey::DutRelayGate(0, 0)),
        "the gate is not focusable while the backend is disabled"
    );
    for k in [
        FieldKey::DutRelayPort(0, 0),
        FieldKey::DutRelayReset(0, 0),
        FieldKey::DutRelayMaskrom(0, 0),
        FieldKey::DutRelayRecovery(0, 0),
        FieldKey::DutRelayPower(0, 0),
        FieldKey::DutRelayResetTimeMs(0, 0),
        FieldKey::DutRelayPowerOffTimeMs(0, 0),
    ] {
        assert!(
            !zone.contains(&k),
            "{k:?} is hidden while the backend is disabled — must not be focusable"
        );
        assert!(
            !state.field_order().contains(&k),
            "{k:?} must not stay in the validation walk while hidden"
        );
    }
}

/// Focus on a DUT-owned advanced TEXT field (target.login_pass) is
/// DUT-zone focus: ←/→ switches the DUT tab group and Up/Down stays
/// inside the DUT field walk (— the moved keys must never
/// fall back to the host zone's pre-move classification).
#[test]
fn test_dut_owned_advanced_focus_stays_in_dut_zone() {
    let mut state = create_state(&InitOptions::default());
    state.add_dut(0); // host 0 now has two DUTs (add_dut auto-selects DUT 1)
    state.selected_dut = Some(0);
    let fk = FieldKey::DutAdvanced(0, 0, "target.login_pass");
    state.set_focus(FocusSlot::Field(fk.clone()));
    // ←/→ from the DUT-owned field switches the DUT tab (not the host).
    handle_form_key(&mut state, key(KeyCode::Right));
    assert_eq!(state.focus, FocusSlot::DutTab(1));
    assert_eq!(state.selected_dut, Some(1));
    state.selected_dut = Some(0);
    state.set_focus(FocusSlot::Field(fk.clone()));
    // Up walks the DUT zone backwards: login pass ← login user (the
    // gate row is gone —).
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(
        state.focus,
        FocusSlot::Field(FieldKey::DutLoginUser(0, 0)),
        "Up from target.login_pass must land on login user, never the host zone"
    );
    // Down walks forward: login pass → login prompt.
    state.set_focus(FocusSlot::Field(fk));
    handle_form_key(&mut state, key(KeyCode::Down));
    assert_eq!(
        state.focus,
        FocusSlot::Field(FieldKey::DutAdvanced(0, 0, "target.login_prompt"))
    );
}

/// Collapsing `[Other ▼]` must NOT steal focus from a DUT-owned
/// advanced field (its row lives in the DUT box, which the collapse
/// does not touch); host-level advanced focus still relocates to the
/// `[Other]` button (their rows disappear with the collapse).
#[test]
fn test_collapse_other_keeps_focus_on_dut_owned_advanced() {
    let mut state = create_state(&InitOptions::default());
    assert!(state.other_expanded);
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    state.set_focus(FocusSlot::Field(fk.clone()));
    state.toggle_other(); // collapse
    assert!(!state.other_expanded);
    assert_eq!(
        state.focus,
        FocusSlot::Field(fk.clone()),
        "collapsing [Other] must not steal focus from a DUT-owned field"
    );
    // Host-level advanced focus still relocates to the [Other] button
    // (serial.baudrate is DUT-owned now, so use a host-level key).
    let hk = FieldKey::Advanced("monitor.hang_timeout");
    state.set_focus(FocusSlot::Field(hk.clone()));
    state.toggle_other(); // re-expand (no relocation on expand)
    state.set_focus(FocusSlot::Field(hk.clone()));
    state.toggle_other(); // collapse again
    assert_eq!(
        state.focus,
        FocusSlot::Button(ButtonId::ToggleOther),
        "a host-level advanced row disappears with the collapse — focus relocates"
    );
}

/// focus_first_error must NOT expand the host panel's advanced block
/// for a DUT-owned key's error (the row is in the DUT box, always
/// reachable by scroll); host-level advanced errors keep the
/// expand-first behavior.
#[test]
fn test_focus_first_error_skips_host_expand_for_dut_owned_key() {
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        create_state(&InitOptions::default()),
    );
    tui.state.toggle_other(); // collapse the host advanced block
    assert!(!tui.state.other_expanded);
    let fk = FieldKey::DutAdvanced(0, 0, "target.login_pass");
    tui.state.errors.insert(fk.clone(), "boom".into());
    tui.focus_first_error();
    assert_eq!(tui.state.focus, FocusSlot::Field(fk.clone()));
    assert!(
        !tui.state.other_expanded,
        "a DUT-owned error must NOT expand the host panel's advanced block"
    );
    // A host-level advanced error still expands the block (the
    // pre-move behavior — its row only exists while expanded;
    // serial.baudrate is DUT-owned now, so use a host-level key).
    let hk = FieldKey::Advanced("monitor.hang_timeout");
    tui.state.errors.clear();
    tui.state.errors.insert(hk.clone(), "boom".into());
    tui.focus_first_error();
    assert!(
        tui.state.other_expanded,
        "a host-level advanced error expands the block first"
    );
    assert_eq!(tui.state.focus, FocusSlot::Field(hk));
}

// ── verified-finding fixes ──────────────────────────────

/// F1: submit_values must stamp DutRelayGate=yes PER DUT from THAT
/// DUT's own control.dev_ctrl (the same predicate the section
/// visibility uses) — the global fallback must not leak across DUTs.
#[test]
fn test_submit_stamps_relay_gate_per_dut() {
    let mut state = create_state(&quick());
    // Global fallback = none, DUT 0 overrides to a real backend: the
    // relay section is VISIBLE for DUT 0 → its gate must be stamped.
    state
        .values
        .insert(FieldKey::Advanced("control.dev_ctrl"), "none".into());
    state.values.insert(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let submitted = state.submit_values();
    assert_eq!(
        submitted
            .get(&FieldKey::DutRelayGate(0, 0))
            .map(String::as_str),
        Some("yes"),
        "a DUT with a real backend gets its gate stamped yes even when the global is none"
    );
    // Reverse: global = ch340-relay, DUT 0 = none (the section is
    // HIDDEN for that DUT) — its gate must NOT be stamped yes.
    let mut state = create_state(&quick());
    state
        .values
        .insert(FieldKey::Advanced("control.dev_ctrl"), "ch340-relay".into());
    state.values.insert(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    let submitted = state.submit_values();
    assert_eq!(
        submitted
            .get(&FieldKey::DutRelayGate(0, 0))
            .map(String::as_str),
        Some("no"),
        "a none/disabled DUT keeps its seeded no gate even when the global backend is real"
    );
    // Unchosen (EMPTY) per-DUT backend: nothing is stamped — a no-op
    // F2 must not fabricate a relay block (the section still SHOWS
    // so the user can choose, but "enabled" means CHOSEN).
    let mut state = create_state(&quick());
    state
        .values
        .insert(FieldKey::Advanced("control.dev_ctrl"), "ch340-relay".into());
    let submitted = state.submit_values();
    assert_eq!(
        submitted
            .get(&FieldKey::DutRelayGate(0, 0))
            .map(String::as_str),
        Some("no"),
        "an unchosen backend stamps nothing"
    );
}

/// H1: the host panel ends AT its last content row — the content
/// height reports the real field rows (the top pad row is chrome),
/// so no trailing blank row sits between the host content and the
/// divider ("DUT content follows IMMEDIATELY"). Sized generously so
/// the host keeps its natural height (a cramped floor would scroll
/// the host instead — the documented DUT-visibility-wins rule).
#[test]
fn test_host_panel_ends_at_last_content_row() {
    let state = create_state(&quick());
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let last_field_bottom = map
        .fields
        .iter()
        .filter(|(k, _)| field_in_host(k))
        .map(|(_, r)| r.bottom())
        .max()
        .unwrap();
    assert_eq!(
        last_field_bottom,
        map.host_panel.bottom(),
        "the last host field row IS the panel's last row (no trailing blank)"
    );
    assert_eq!(
        map.host_panel_inner.height as usize,
        map.host_content_height + 1,
        "the bottom-open interior covers the pad row + every content row"
    );
    assert!(
        map.host_scrollbar.is_none(),
        "a hugging host panel never shows a scrollbar"
    );
}

/// H2: the DUT tab bar sits on the panel's TOP row (no blank row
/// between the divider and the tab bar) and the panel is exactly
/// content + 4 chrome rows (tab + 2 box borders + bottom border).
#[test]
fn test_dut_tab_bar_sits_on_panel_top_row() {
    let state = create_state(&quick());
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert_eq!(
        map.dut_tab_bar.y, map.dut_panel.y,
        "the tab bar starts on the panel's first row"
    );
    assert_eq!(
        map.dut_content.y,
        map.dut_panel.y.saturating_add(1),
        "the content box starts on the panel's second row"
    );
    assert_eq!(
        map.dut_panel.height as usize,
        map.dut_content_height + 4,
        "the DUT panel hugs its content (1 tab + 2 box borders + content + bottom border)"
    );
    assert_eq!(
        map.dut_content_inner.height as usize, map.dut_content_height,
        "the box interior hugs the content"
    );
}

/// F3: the amber shrink notes are LAID-OUT rows — the layout extends
/// the content height and reports the note rect, so a hugging panel
/// never overlaps the last field row with the note.
#[test]
fn test_shrink_notes_are_laid_out_rows() {
    let _guard = lock_env();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::remove_var("TARGET_CONF");
    }
    const FIXTURE: &str = r#"{
  "dev_hosts": [
    {
      "ip": "10.0.0.1", "user": "", "pass": "",
      "duts": [
        { "dut_name": "dut1", "serial": { "port": 41002 } },
        { "dut_name": "dut2", "serial": { "port": 41003 } }
      ]
    },
    { "ip": "10.0.0.2", "user": "", "pass": "", "duts": [] }
  ]
}"#;
    let path = tmp.path().join(".target.jsonc");
    std::fs::write(&path, FIXTURE).unwrap();
    let prepared = prepare_init(tmp.path(), &quick()).unwrap();
    // DUT-count shrink: the note becomes the last content row.
    let mut state = FormState::new(
        prepared.schema.clone(),
        prepared.target_path.display().to_string(),
    );
    state.remove_dut(0, 1);
    let map = form_layout(Rect::new(0, 0, 80, 40), &state);
    let note_rect = map.dut_note.expect("the DUT note is laid out");
    assert_eq!(
        note_rect.y,
        map.dut_content_inner.bottom().saturating_sub(1),
        "the note row is the box interior's last row"
    );
    let last_field_bottom = map
        .fields
        .iter()
        .filter(|(k, _)| field_dut_index(k).is_some())
        .map(|(_, r)| r.bottom())
        .max()
        .unwrap();
    assert!(
        last_field_bottom <= note_rect.y,
        "the note sits at or below the last field row (no overwrite)"
    );
    // Host-count shrink: the note is the host panel's last row.
    let mut state = FormState::new(
        prepared.schema.clone(),
        prepared.target_path.display().to_string(),
    );
    state.remove_host(1);
    let map = form_layout(Rect::new(0, 0, 80, 40), &state);
    let note_rect = map.host_note.expect("the host note is laid out");
    assert_eq!(
        note_rect.y,
        map.host_panel.bottom().saturating_sub(1),
        "the note row is the host panel's last row"
    );
    let last_field_bottom = map
        .fields
        .iter()
        .filter(|(k, _)| field_in_host(k))
        .map(|(_, r)| r.bottom())
        .max()
        .unwrap();
    assert!(
        last_field_bottom <= note_rect.y,
        "the host note sits at or below the last field row (no overwrite)"
    );
}

/// The DUT box is a LEFT/RIGHT two-column split at any width
/// ≥60 (deliberate — no 3/4-column compression).
#[test]
fn test_dut_two_columns_left_right_split() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    ); // relay ON → the full 5-box content (18 rows at 2 columns)
    let map80 = form_layout(Rect::new(0, 0, 80, 40), &state);
    assert_eq!(
        map80.separators.len(),
        1,
        "80 cols → one separator (left/right)"
    );
    assert_eq!(
        map80.separators[0].x,
        map80.dut_content_inner.x + map80.dut_content_inner.width / 2
    );
    assert_eq!(
        map80.dut_content_height, 18,
        "the relay-ON box at 2 columns (title + pinned boxes)"
    );
    let map79 = form_layout(Rect::new(0, 0, 79, 40), &state);
    assert_eq!(map79.separators.len(), 1, "still two columns at 79");
}

// ── DUT field boxes ────────────────────────────────────

/// The DUT content box groups its fields into five visual BOXES in
/// canonical order: identity → relay → flash → console → monitor.
/// Membership is exact and ordered (the relay box is the requirement's
/// verbatim list: dev_ctrl, port, the four channels, the two (ms)
/// timings; the flash box concentrates ALL flash.* keys).
#[test]
fn test_dut_boxes_identity_relay_flash_console_monitor() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let boxes = dut_boxes(&state, 0, 0);
    let names: Vec<&str> = boxes.iter().map(|b| b.name).collect();
    assert_eq!(names, ["identity", "relay", "flash", "console", "monitor"]);
    let adv = |k: &'static str| FieldKey::DutAdvanced(0, 0, k);
    assert_eq!(
        boxes[0].fields,
        [
            FieldKey::DutName(0, 0),
            FieldKey::DutPort(0, 0),
            adv("serial.baudrate"),
            FieldKey::DutLoginUser(0, 0),
            adv("target.login_pass"),
            adv("target.login_prompt"),
        ],
        "identity: static addressing + credentials, anchors the panel"
    );
    assert_eq!(
        boxes[1].fields,
        [
            adv("control.dev_ctrl"),
            FieldKey::DutRelayPort(0, 0),
            FieldKey::DutRelayReset(0, 0),
            FieldKey::DutRelayMaskrom(0, 0),
            FieldKey::DutRelayRecovery(0, 0),
            FieldKey::DutRelayPower(0, 0),
            FieldKey::DutRelayResetTimeMs(0, 0),
            FieldKey::DutRelayPowerOffTimeMs(0, 0),
        ],
        "relay: the requirement's exact 8-member list, one group"
    );
    assert_eq!(
        boxes[2].fields,
        [
            adv("flash.tool"),
            adv("flash.loader_bin"),
            adv("flash.loader_cmd"),
            adv("flash.list_devices_cmd"),
            flash::devices_key(0, 0),
            adv("flash.full_image_cmd"),
            adv("flash.kernel_image_cmd"),
            adv("flash.upload_dir"),
        ],
        "flash: ALL flash.* keys + the device-list row below list_devices_cmd"
    );
    assert_eq!(
        boxes[3].fields,
        [
            adv("loader"),
            adv("uboot.interrupt_char"),
            adv("uboot.interrupt_strategy"),
        ],
        "console: bootloader choice + interaction tuning"
    );
    assert_eq!(
        boxes[4].fields,
        [
            adv("monitor.reference_log"),
            adv("monitor.max_archived_logs"),
            adv("monitor.crash_keywords"),
        ],
        "monitor: the runtime observation trio"
    );
}

#[test]
fn test_pattern_strategy_shows_required_pattern_editor() {
    let mut state = create_state(&quick());
    let adv = |key: &'static str| FieldKey::DutAdvanced(0, 0, key);
    let console_fields = |state: &FormState| -> Vec<FieldKey> {
        dut_boxes(state, 0, 0)
            .into_iter()
            .find(|group| group.name == "console")
            .unwrap()
            .fields
    };

    assert!(!console_fields(&state).contains(&adv("uboot.pattern")));
    state.set_value(adv("uboot.interrupt_strategy"), "pattern".into());
    assert!(console_fields(&state).contains(&adv("uboot.pattern")));

    let errors = resolve_answers(
        &state.schema,
        &state.submit_values(),
        state.gate,
        NamePolicy::ErrorOnCollision,
    )
    .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|(key, message)| { key == &adv("uboot.pattern") && message.contains("required") })
    );

    state.set_value(adv("uboot.pattern"), "Hit any key".into());
    let remaining = resolve_answers(
        &state.schema,
        &state.submit_values(),
        state.gate,
        NamePolicy::ErrorOnCollision,
    )
    .unwrap_err();
    assert!(
        remaining
            .iter()
            .all(|(key, _)| key != &adv("uboot.pattern")),
        "valid pattern must clear its conditional error: {remaining:?}"
    );
}

/// The U-Boot interrupt knobs are hidden when the loader is the
/// not-yet-implemented "uefi" stub (they only apply to U-Boot);
/// uboot (or an unset loader) keeps them.
#[test]
fn test_uefi_loader_hides_uboot_interrupt_knobs() {
    let mut state = create_state(&quick());
    let adv = |k: &'static str| FieldKey::DutAdvanced(0, 0, k);
    let console_fields = |state: &FormState| -> Vec<FieldKey> {
        dut_boxes(state, 0, 0)
            .into_iter()
            .find(|b| b.name == "console")
            .unwrap()
            .fields
    };
    // Default (unset loader): all three console fields show.
    assert_eq!(
        console_fields(&state),
        [
            adv("loader"),
            adv("uboot.interrupt_char"),
            adv("uboot.interrupt_strategy")
        ]
    );
    // loader = uefi: the U-Boot interrupt knobs disappear.
    state.set_value(adv("loader"), "uefi".into());
    assert_eq!(console_fields(&state), [adv("loader")]);
    // loader = uboot: they come back.
    state.set_value(adv("loader"), "uboot".into());
    assert_eq!(
        console_fields(&state),
        [
            adv("loader"),
            adv("uboot.interrupt_char"),
            adv("uboot.interrupt_strategy")
        ]
    );
}

/// The read-only flash device-list row: present in the flash box,
/// excluded from focus navigation, and its value cell renders the
/// probe detail (or a placeholder).
#[test]
fn test_flash_devices_row_shows_detail_and_not_focusable() {
    let mut state = create_state(&quick());
    let fk = flash::devices_key(0, 0);
    // Empty (unprobed) row: label + empty value cell (same style as
    // the other empty flash rows; no "[TEST]" text).
    let terminal = terminal_with(&mut state, 120, 64);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(joined.contains("flash.devices"), "{joined}");
    assert!(!joined.contains("[TEST]"), "{joined}");
    assert!(
        dut_boxes(&state, 0, 0)
            .iter()
            .find(|b| b.name == "flash")
            .unwrap()
            .fields
            .contains(&fk),
        "the device-list row lives in the flash box"
    );
    assert!(
        !state.dut_zone_fields().contains(&fk),
        "the read-only row is never focusable"
    );
    state
        .probe_details
        .insert(fk.clone(), "rkusb: rockusb device".into());
    state.probes.insert(fk.clone(), ProbeMark::Ok);
    let terminal = terminal_with(&mut state, 120, 40);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(joined.contains("rkusb: rockusb device"), "{joined}");
    assert!(joined.contains("flash.devices"), "{joined}");
}

/// dev_ctrl = none keeps ONLY dev_ctrl in the relay box (the control
/// that re-enables the backend must stay visible and focusable); the
/// 7 relay sub-fields disappear from the flat order (never focus an
/// invisible rect) and the other four boxes are unchanged.
#[test]
fn test_dut_boxes_none_backend_keeps_dev_ctrl_only_relay_box() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    let boxes = dut_boxes(&state, 0, 0);
    let names: Vec<&str> = boxes.iter().map(|b| b.name).collect();
    assert_eq!(names, ["identity", "relay", "flash", "console", "monitor"]);
    let adv = |k: &'static str| FieldKey::DutAdvanced(0, 0, k);
    // Under none ALL 8 rows keep their slots (constant box height);
    // the renderer leaves the 7 sub-rows BLANK.
    assert_eq!(
        boxes[1].fields.len(),
        8,
        "none: the relay box keeps ALL row slots"
    );
    let flat: Vec<FieldKey> = boxes.iter().flat_map(|b| b.fields.clone()).collect();
    for k in [
        FieldKey::DutRelayPort(0, 0),
        FieldKey::DutRelayReset(0, 0),
        FieldKey::DutRelayMaskrom(0, 0),
        FieldKey::DutRelayRecovery(0, 0),
        FieldKey::DutRelayPower(0, 0),
        FieldKey::DutRelayResetTimeMs(0, 0),
        FieldKey::DutRelayPowerOffTimeMs(0, 0),
    ] {
        assert!(
            flat.contains(&k),
            "{k:?} keeps its reserved slot while the backend is none"
        );
        assert!(
            relay_row_disabled(&state, &k),
            "{k:?} is classified disabled (blank + unfocusable)"
        );
    }
    // The other four boxes are unchanged by the backend choice.
    assert_eq!(
        boxes[0].fields,
        [
            FieldKey::DutName(0, 0),
            FieldKey::DutPort(0, 0),
            adv("serial.baudrate"),
            FieldKey::DutLoginUser(0, 0),
            adv("target.login_pass"),
            adv("target.login_prompt"),
        ]
    );
    assert_eq!(boxes[2].fields.len(), 8, "flash box keeps its rows");
    assert_eq!(
        boxes[3].fields,
        [
            adv("loader"),
            adv("uboot.interrupt_char"),
            adv("uboot.interrupt_strategy"),
        ]
    );
    assert_eq!(boxes[4].fields.len(), 3, "monitor box unchanged");
}

/// Each box starts with one dim dashed TITLE rule (`── name ─…`)
/// rendered via the existing key-less dim RowSpec branch: exactly five
/// of them (one per box), each the FIRST row of its box (directly
/// above the box's first field) and spanning its column's full width.
#[test]
fn test_dut_box_titles_render_dashed_title_rules() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let titles: Vec<&RowSpec> = map
        .dut_rows
        .iter()
        .filter(|r| r.key.is_none() && r.dim && r.label.starts_with("── "))
        .collect();
    assert_eq!(
        titles.len(),
        5,
        "one dashed title per box: {:?}",
        titles.iter().map(|t| &t.label).collect::<Vec<_>>()
    );
    for name in ["identity", "relay", "flash", "console", "monitor"] {
        assert!(
            titles
                .iter()
                .any(|t| t.label.starts_with(&format!("── {name}"))),
            "a {name} title row exists"
        );
    }
    // Each title is directly above its box's first field, sharing the
    // box's column span (col label x .. col end) — a full-width rule.
    let inner = map.dut_content_inner;
    let lx = inner.x.saturating_add(2);
    let sep_x = inner.x + inner.width / 2;
    let bounds = |c: usize| -> (u16, u16) {
        if c == 0 {
            (lx, sep_x.saturating_sub(1))
        } else {
            (sep_x.saturating_add(3), inner.right().saturating_sub(1))
        }
    };
    let rect = |k: &FieldKey| {
        map.fields
            .iter()
            .find(|(f, _)| f == k)
            .unwrap_or_else(|| panic!("missing {k:?}"))
            .1
    };
    for b in dut_boxes(&state, 0, 0) {
        let first = rect(&b.fields[0]);
        let c = usize::from(first.x >= sep_x);
        let (cstart, cend) = bounds(c);
        let title = titles
            .iter()
            .find(|t| t.value_rect.x == cstart && t.value_rect.y == first.y.saturating_sub(1))
            .unwrap_or_else(|| panic!("missing title for box {}", b.name));
        assert_eq!(
            title.value_rect, title.label_rect,
            "{}: the title's label and value rects coincide",
            b.name
        );
        assert_eq!(
            title.value_rect.x, cstart,
            "{}: starts at the column label x",
            b.name
        );
        assert_eq!(
            title.value_rect.width,
            cend.saturating_sub(cstart).saturating_add(1),
            "{}: spans the whole column",
            b.name
        );
    }
    // The rendered buffer shows the dashed titles.
    let terminal = terminal_with(&mut state, 120, 40);
    let joined = buffer_rows(&terminal).join("\n");
    assert!(joined.contains("── relay"), "{joined}");
    assert!(joined.contains("── flash"), "{joined}");
}

/// Boxes are PINNED to columns — identity + relay LEFT, flash +
/// console + monitor RIGHT (the top-to-bottom / left-to-right reading
/// order). No box ever splits across the separator, and inside a
/// column the boxes stack consecutively in canonical order.
#[test]
fn test_dut_boxes_pinned_to_columns_left_right() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let inner = map.dut_content_inner;
    assert_eq!(map.separators.len(), 1, "the left/right split stays");
    assert_eq!(map.separators[0].x, inner.x + inner.width / 2);
    let sep_x = map.separators[0].x;
    let rect = |k: &FieldKey| {
        map.fields
            .iter()
            .find(|(f, _)| f == k)
            .unwrap_or_else(|| panic!("missing {k:?}"))
            .1
    };
    for b in dut_boxes(&state, 0, 0) {
        let expected = match b.name {
            "identity" | "relay" => 0,
            "flash" | "console" | "monitor" => 1,
            other => panic!("unexpected box {other}"),
        };
        let first = rect(&b.fields[0]);
        for (i, k) in b.fields.iter().enumerate() {
            let r = rect(k);
            let col = if r.x < sep_x {
                0
            } else if r.x > sep_x {
                1
            } else {
                panic!("{k:?} sits ON the separator")
            };
            assert_eq!(col, expected, "{k:?} pinned to the wrong column");
            assert_eq!(
                r.y,
                first.y.saturating_add(i as u16),
                "{k:?} must stack consecutively inside its box"
            );
        }
    }
    // Within a column the boxes stack in canonical order.
    let boxes = dut_boxes(&state, 0, 0);
    let left: Vec<_> = boxes
        .iter()
        .filter(|b| matches!(b.name, "identity" | "relay"))
        .collect();
    let right: Vec<_> = boxes
        .iter()
        .filter(|b| matches!(b.name, "flash" | "console" | "monitor"))
        .collect();
    assert!(
        rect(&left[0].fields[0]).y < rect(&left[1].fields[0]).y,
        "identity above relay"
    );
    for pair in right.windows(2) {
        assert!(
            rect(&pair[0].fields[0]).y < rect(&pair[1].fields[0]).y,
            "{} above {}",
            pair[0].name,
            pair[1].name
        );
    }
}

/// The box layout: relay-ON = 17 rows at every 2-col width
/// (1 title + 16 box rows). Backend none keeps the SAME height —
/// the 7 sub-rows stay as blank reserved slots (nothing SHOWS below
/// dev_ctrl, and switching can never jump the layout —).
/// The interior hugs exactly at 120x40 (no scrollbar, panel = +4);
/// the 80x24 relay-ON box still overflows → scrollbar.
#[test]
fn test_dut_box_content_heights_hug() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert_eq!(map.dut_content_height, 18, "relay-ON 2-col height");
    assert_eq!(
        map.dut_content_inner.height as usize, map.dut_content_height,
        "the box interior hugs the content"
    );
    assert_eq!(
        map.dut_panel.height as usize,
        map.dut_content_height + 4,
        "the DUT panel hugs its content"
    );
    assert!(map.dut_scrollbar.is_none(), "no scrollbar at 120x40");
    // backend none: the sub-rows keep their BLANK slots — the height
    // is IDENTICAL (no-jump invariant).
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "none".into(),
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    assert_eq!(
        map.dut_content_height, 18,
        "backend-none height UNCHANGED — no layout jump"
    );
    // 80x24 relay-ON: 19 rows overflow the cramped box → scrollbar.
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let map = form_layout(Rect::new(0, 0, 80, 24), &state);
    assert!(map.dut_scrollbar.is_some(), "80x24 relay-ON overflows");
}

/// Box titles are dead chrome: never focusable, never click targets
/// (key-less dim rows OUTSIDE map.fields by construction — the mouse
/// hit-test contract is untouched).
#[test]
fn test_dut_box_titles_not_focusable_or_clickable() {
    let mut state = create_state(&quick());
    state.set_value(
        FieldKey::DutAdvanced(0, 0, "control.dev_ctrl"),
        "ch340-relay".into(),
    );
    let map = form_layout(Rect::new(0, 0, 120, 40), &state);
    let titles: Vec<Rect> = map
        .dut_rows
        .iter()
        .filter(|r| r.key.is_none() && r.dim && r.label.starts_with("── "))
        .map(|r| r.value_rect)
        .collect();
    assert_eq!(titles.len(), 5, "one title per box");
    for t in &titles {
        assert!(
            !map.fields.iter().any(|(_, r)| r.intersects(*t)),
            "no field rect may overlap a title row {t:?}"
        );
    }
    // Clicking a title cell selects no field and produces no action.
    let mut tui = FormTui::new(
        Terminal::new(TestBackend::new(120, 40)).unwrap(),
        KeyQueue::new(Vec::new()),
        state,
    );
    let before = tui.state.focus.clone();
    let (tx, ty) = (titles[0].x, titles[0].y);
    let res = tui.handle_mouse(mouse_down(tx, ty)).unwrap();
    assert!(res.is_none(), "a title click must produce no action");
    assert_eq!(tui.state.focus, before, "a title click must not move focus");
}
// ── Select Up cycling ───────────────────────────────

/// Up on the FIRST option wraps to the LAST (mirror of the Down
/// wrap) — the dropdown cycles in both directions (the
/// dev_ctrl options are ch340-relay and none only).
#[test]
fn test_up_cycles_select_backwards_with_wrap() {
    let mut state = create_state(&InitOptions::default());
    let fk = FieldKey::DutAdvanced(0, 0, "control.dev_ctrl");
    state.set_focus(FocusSlot::Field(fk.clone()));
    handle_form_key(&mut state, chr('1')); // ch340-relay (first)
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(
        state.values.get(&fk).map(String::as_str),
        Some("none"),
        "Up from the first option wraps to the last"
    );
    handle_form_key(&mut state, key(KeyCode::Up));
    assert_eq!(
        state.values.get(&fk).map(String::as_str),
        Some("ch340-relay"),
        "Up cycles back to the first option"
    );
}

/// The endpoint-holder snapshot surfaces INLINE in the DUT title only
/// DURING a TEST run (because it blocks the temporary test server). Outside a run
/// the title stays plain: this project's own MCP holding the endpoint is
/// the normal state, not a warning. Matching is per (host ip, serial port).
#[test]
fn dut_title_surfaces_endpoint_holder_inline() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lock_dir = tmp.path().join("locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_dir = lock_dir.to_str().unwrap().to_string();
    // A LIVE holder (our own pid) on 10.0.0.1:41002 — the exact endpoint
    // the form below edits.
    let lock_path = sermcp::lock_manager::endpoint_lock_path("10.0.0.1", "41002", &lock_dir);
    std::fs::write(
        &lock_path,
        format!(
            "{}\n10.0.0.1:41002\nhold\n2026-01-01T00:00:00+00:00\n",
            std::process::id()
        ),
    )
    .unwrap();

    let opts = InitOptions::default();
    let mut state = FormState::new(build_form_schema(None, &opts), "t".into());
    state.endpoint_lock_dir = lock_dir.clone();
    state.host_count = 1;
    state.dut_counts = vec![1];
    state.selected_host = 0;
    state.selected_dut = Some(0);
    state.values.insert(FieldKey::HostIp(0), "10.0.0.1".into());
    state.values.insert(FieldKey::DutPort(0, 0), "41002".into());

    state.refresh_endpoint_conflicts();
    let rect = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    // OUTSIDE a TEST run the title is plain: this project's own MCP
    // holding the endpoint is the NORMAL state, not a warning — a
    // permanent ⚠ on the idle form is noise.
    let (rows, ..) = dut_content_layout(&state, rect);
    assert!(
        !rows[0].label.contains("⚠"),
        "idle form must not warn about the holder: {}",
        rows[0].label
    );

    // DURING a TEST run a pre-existing holder surfaces in the title because
    // the temporary form-snapshot server cannot own the endpoint.
    let (_tx, rx) = std::sync::mpsc::channel();
    state.test_run = Some(TestRun {
        rx,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
    });
    let (rows, ..) = dut_content_layout(&state, rect);
    let title = &rows[0].label;
    assert!(
        title.contains("⚠ held by PID"),
        "holder must surface during a TEST run: {title}"
    );
    assert!(
        title.contains(&std::process::id().to_string()),
        "holder pid must surface: {title}"
    );

    state.test_owned_pids.insert(std::process::id());
    let (rows, ..) = dut_content_layout(&state, rect);
    assert!(
        !rows[0].label.contains("⚠"),
        "the TEST's own MCP must not be reported as a conflict: {}",
        rows[0].label
    );
    state.test_owned_pids.clear();

    // A different serial port has its own lock — no warning even in a
    // TEST run.
    state.values.insert(FieldKey::DutPort(0, 0), "41003".into());
    state.refresh_endpoint_conflicts();
    let (rows, ..) = dut_content_layout(&state, rect);
    assert!(
        !rows[0].label.contains("⚠"),
        "an unheld endpoint must render a plain title: {}",
        rows[0].label
    );
}
