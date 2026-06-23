//! Power control abstraction — trait + implementations.
//!
//! Provides a unified interface for controlling DUT power/reset/maskrom/recovery
//! across different hardware backends (CH340 relay, SNMP PDU, software reboot,
//! GPIO button simulation).
//!
//! # Design
//!
//! The `PowerControl` trait abstracts physical button operations (press/release)
//! so the rest of the system never needs to know whether a "reset" is done via
//! a CH340 relay, an SNMP PDU, or a `reboot` command over serial.
//!
//! # Implementations
//!
//! - `Ch340RelayControl`: 4-byte TCP protocol via ser2net, with read-back verify.
//! - `SoftwareRebootControl`: sends `reboot` over serial (no hardware control).
//! - `SnmpPduControl`: future — SNMP v3 SET/GET on OID.
//! - `ButtonSimControl`: future — GPIO line control via /dev/gpiochipN.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Physical buttons that can be pressed/released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Reset,
    Maskrom,
    Recovery,
}

impl Button {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Button::Reset => "reset",
            Button::Maskrom => "maskrom",
            Button::Recovery => "recovery",
        }
    }
}

/// Errors from power control operations.
#[derive(Debug)]
pub enum PowerError {
    NotConfigured,
    NotConnected,
    VerifyFailed { sent: u8, read_back: u8 },
    Io(std::io::Error),
    Timeout,
}

impl std::fmt::Display for PowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowerError::NotConfigured => write!(f, "power control not configured"),
            PowerError::NotConnected => write!(f, "power control not connected"),
            PowerError::VerifyFailed { sent, read_back } => {
                write!(f, "relay verify failed: sent {sent:#04x}, read back {read_back:#04x}")
            }
            PowerError::Io(e) => write!(f, "IO error: {e}"),
            PowerError::Timeout => write!(f, "operation timed out"),
        }
    }
}

impl std::error::Error for PowerError {}

impl From<std::io::Error> for PowerError {
    fn from(e: std::io::Error) -> Self {
        PowerError::Io(e)
    }
}

/// Power control trait — abstracts physical button operations.
///
/// All methods are async because backends may involve network I/O (TCP relay,
/// SNMP) or serial I/O (software reboot).
#[allow(async_fn_in_trait)]
pub trait PowerControl: Send + Sync {
    /// Press a button (set pin low / turn relay ON / PDU on).
    async fn press(&mut self, button: Button) -> Result<(), PowerError>;

    /// Release a button (set pin high / turn relay OFF / PDU off).
    async fn release(&mut self, button: Button) -> Result<(), PowerError>;

    /// Pulse: press → delay → release.
    async fn pulse(&mut self, button: Button, delay_ms: u64) -> Result<(), PowerError> {
        self.press(button).await?;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        self.release(button).await
    }

    /// Verify the control is actually working (read back state).
    /// Returns `Ok(true)` if verified, `Ok(false)` if not verifiable (e.g.
    /// software reboot has no read-back), `Err` on error.
    async fn verify(&mut self) -> Result<bool, PowerError>;

    /// Human-readable name of the backend (e.g. "ch340-relay", "software-reboot").
    fn name(&self) -> &'static str;

    /// Whether this backend controls the given button.
    fn has_button(&self, button: Button) -> bool;
}

// ── Ch340RelayControl ──────────────────────────────────────────────────────

/// CH340 relay control via TCP (ser2net). 4-byte protocol:
/// `[0xA0, channel, opcode, checksum]` where checksum = `(0xA0 + ch + op) & 0xFF`.
///
/// Channels are 1-indexed (user-facing channel 1 = protocol channel 1).
/// Read-back verification: after sending ON/OFF, send STATUS and compare.
pub struct Ch340RelayControl {
    host: String,
    port: u16,
    channels: ButtonChannels,
    stream: Option<TcpStream>,
    /// If true, ON(0x01) and OFF(0x00) are swapped
    inverted: bool,
}

/// Mapping of buttons to relay channels (1-4). 0 = not configured.
#[derive(Debug, Clone, Default)]
pub struct ButtonChannels {
    pub reset: u8,
    pub maskrom: u8,
    pub recovery: u8,
}

impl ButtonChannels {
    pub fn get(&self, button: Button) -> u8 {
        match button {
            Button::Reset => self.reset,
            Button::Maskrom => self.maskrom,
            Button::Recovery => self.recovery,
        }
    }

    pub fn has_any(&self) -> bool {
        self.reset > 0 || self.maskrom > 0 || self.recovery > 0
    }
}

impl Ch340RelayControl {
    const HEADER: u8 = 0xA0;
    const OP_ON: u8 = 0x01;
    const OP_OFF: u8 = 0x00;
    const OP_STATUS: u8 = 0x05;

    pub fn new(host: String, port: u16, channels: ButtonChannels) -> Self {
        Self { host, port, channels, stream: None, inverted: false }
    }

    pub fn with_inverted(mut self, inverted: bool) -> Self {
        self.inverted = inverted;
        self
    }

    pub fn is_configured(&self) -> bool {
        self.port > 0 && self.channels.has_any()
    }

    async fn ensure_open(&mut self) -> Result<(), PowerError> {
        if self.stream.is_none() {
            let addr = format!("{}:{}", self.host, self.port);
            let stream = tokio::time::timeout(
                Duration::from_secs(5),
                TcpStream::connect(&addr),
            )
            .await
            .map_err(|_| PowerError::Timeout)?
            .map_err(PowerError::Io)?;
            stream.set_nodelay(true).ok();
            self.stream = Some(stream);
        }
        Ok(())
    }

    async fn force_reconnect(&mut self) -> Result<(), PowerError> {
        self.stream.take();
        self.ensure_open().await
    }

    /// Send a 4-byte command and drain any response.
    async fn send_command(
        &mut self,
        channel: u8,
        opcode: u8,
    ) -> Result<Vec<u8>, PowerError> {
        let checksum = (Self::HEADER as u16 + channel as u16 + opcode as u16) & 0xFF;
        let packet = [Self::HEADER, channel, opcode, checksum as u8];

        for attempt in 0..2 {
            self.ensure_open().await.map_err(|e| {
                if attempt == 0 {
                    PowerError::NotConnected
                } else {
                    e
                }
            })?;

            let stream = self.stream.as_mut().unwrap();
            match stream.write_all(&packet).await {
                Ok(_) => {}
                Err(_) if attempt == 0 => {
                    self.force_reconnect().await?;
                    continue;
                }
                Err(e) => return Err(PowerError::Io(e)),
            }
            stream.flush().await?;

            tokio::time::sleep(Duration::from_millis(150)).await;

            if opcode == Self::OP_STATUS {
                let mut buf = [0u8; 256];
                match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await {
                    Ok(Ok(n)) => return Ok(buf[..n].to_vec()),
                    Ok(Err(e)) => return Err(PowerError::Io(e)),
                    Err(_) => return Ok(Vec::new()),
                }
            }

            // Drain any response for ON/OFF commands (ser2net banner + relay echo).
            let mut drain = [0u8; 256];
            let _ = tokio::time::timeout(Duration::from_millis(200), stream.read(&mut drain)).await;
            return Ok(Vec::new());
        }
        Err(PowerError::NotConnected)
    }

    /// Read back the status of a channel.
    async fn read_channel_state(&mut self, channel: u8) -> Result<u8, PowerError> {
        let resp = self.send_command(channel, Self::OP_STATUS).await?;
        // Response format: [0xA0, channel, status, checksum]
        // status: 0x01 = ON, 0x00 = OFF
        if resp.len() >= 3 {
            Ok(resp[2])
        } else {
            Err(PowerError::VerifyFailed { sent: 0, read_back: 0 })
        }
    }
}

impl PowerControl for Ch340RelayControl {
    async fn press(&mut self, button: Button) -> Result<(), PowerError> {
        let ch = self.channels.get(button);
        if ch == 0 {
            return Err(PowerError::NotConfigured);
        }
        self.send_command(ch, Self::OP_ON).await.map(|_| ())
    }

    async fn release(&mut self, button: Button) -> Result<(), PowerError> {
        let ch = self.channels.get(button);
        if ch == 0 {
            return Err(PowerError::NotConfigured);
        }
        self.send_command(ch, Self::OP_OFF).await.map(|_| ())
    }

    async fn verify(&mut self) -> Result<bool, PowerError> {
        if !self.is_configured() {
            return Err(PowerError::NotConfigured);
        }
        // Verify by toggling the reset channel and reading back.
        let ch = if self.channels.reset > 0 {
            self.channels.reset
        } else {
            // Use the first available channel for verification.
            if self.channels.maskrom > 0 {
                self.channels.maskrom
            } else {
                self.channels.recovery
            }
        };
        if ch == 0 {
            return Ok(false); // No channel to verify with
        }

        // Turn ON and read back.
        self.send_command(ch, Self::OP_ON).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let state_on = self.read_channel_state(ch).await?;

        // Turn OFF and read back.
        self.send_command(ch, Self::OP_OFF).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let state_off = self.read_channel_state(ch).await?;

        let verified = state_on == Self::OP_ON && state_off == Self::OP_OFF;
        if !verified {
            return Err(PowerError::VerifyFailed {
                sent: Self::OP_ON,
                read_back: state_on,
            });
        }
        Ok(true)
    }

    fn name(&self) -> &'static str {
        "ch340-relay"
    }

    fn has_button(&self, button: Button) -> bool {
        self.channels.get(button) > 0
    }
}

// ── SoftwareRebootControl ──────────────────────────────────────────────────

/// Software reboot control — sends `reboot` over serial.
/// No hardware button control. Only supports "reset" as a soft reboot.
#[allow(dead_code)]
pub struct SoftwareRebootControl;

impl SoftwareRebootControl {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

impl PowerControl for SoftwareRebootControl {
    async fn press(&mut self, button: Button) -> Result<(), PowerError> {
        if button != Button::Reset {
            return Err(PowerError::NotConfigured);
        }
        // The actual reboot command is sent by the serial engine.
        // This is a no-op here — the engine handles it.
        Ok(())
    }

    async fn release(&mut self, _button: Button) -> Result<(), PowerError> {
        Ok(())
    }

    async fn verify(&mut self) -> Result<bool, PowerError> {
        Ok(false) // Not verifiable via hardware
    }

    fn name(&self) -> &'static str {
        "software-reboot"
    }

    fn has_button(&self, button: Button) -> bool {
        button == Button::Reset
    }
}

// ── ExternalControl ────────────────────────────────────────────────────────

/// External device control program (e.g. `embedded-dev-ctl`).
///
/// Executes a configured external binary to control DUT buttons.
/// Protocol (as specified in tech-design.md §6.3):
///
/// ```bash
/// embedded-dev-ctl --dut <alias> button reset --pressed true
/// embedded-dev-ctl --dut <alias> button reset --pressed false
/// embedded-dev-ctl --dut <alias> verify
/// ```
pub struct ExternalControl {
    program: String,
    dut_alias: String,
    /// Button channel mapping (passed as arguments to the external program).
    channels: ButtonChannels,
}

impl ExternalControl {
    pub fn new(program: String, dut_alias: String, channels: ButtonChannels) -> Self {
        Self {
            program,
            dut_alias,
            channels,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.program.is_empty() && self.channels.has_any()
    }

    async fn run(&self, args: &[&str]) -> Result<String, PowerError> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(args);
        cmd.arg("--dut");
        cmd.arg(&self.dut_alias);

        let output = cmd.output().await.map_err(|e| {
            PowerError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("External program '{}' failed: {e}", self.program),
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PowerError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("External program '{}' exit {}: {stderr}", self.program, output.status),
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn button_arg(&self, button: Button) -> &'static str {
        match button {
            Button::Reset => "reset",
            Button::Maskrom => "maskrom",
            Button::Recovery => "recovery",
        }
    }
}

impl PowerControl for ExternalControl {
    async fn press(&mut self, button: Button) -> Result<(), PowerError> {
        if !self.has_button(button) {
            return Err(PowerError::NotConfigured);
        }
        self.run(&["button", self.button_arg(button), "--pressed", "true"]).await?;
        Ok(())
    }

    async fn release(&mut self, button: Button) -> Result<(), PowerError> {
        if !self.has_button(button) {
            return Err(PowerError::NotConfigured);
        }
        self.run(&["button", self.button_arg(button), "--pressed", "false"]).await?;
        Ok(())
    }

    async fn verify(&mut self) -> Result<bool, PowerError> {
        if !self.is_configured() {
            return Err(PowerError::NotConfigured);
        }
        self.run(&["verify"]).await?;
        Ok(true)
    }

    fn name(&self) -> &'static str {
        "external-dev-ctl"
    }

    fn has_button(&self, button: Button) -> bool {
        self.channels.get(button) > 0
    }
}

// ── PowerControlBackend enum ───────────────────────────────────────────────

/// Enum-based dispatch for power control backends.
/// Avoids `dyn PowerControl` (which isn't possible with async fn in trait).
pub enum PowerControlBackend {
    Ch340(Ch340RelayControl),
    External(ExternalControl),
    Software(SoftwareRebootControl),
}

impl PowerControlBackend {
    pub async fn press(&mut self, button: Button) -> Result<(), PowerError> {
        match self {
            PowerControlBackend::Ch340(c) => c.press(button).await,
            PowerControlBackend::External(c) => c.press(button).await,
            PowerControlBackend::Software(c) => c.press(button).await,
        }
    }

    pub async fn release(&mut self, button: Button) -> Result<(), PowerError> {
        match self {
            PowerControlBackend::Ch340(c) => c.release(button).await,
            PowerControlBackend::External(c) => c.release(button).await,
            PowerControlBackend::Software(c) => c.release(button).await,
        }
    }

    pub async fn pulse(&mut self, button: Button, delay_ms: u64) -> Result<(), PowerError> {
        match self {
            PowerControlBackend::Ch340(c) => c.pulse(button, delay_ms).await,
            PowerControlBackend::External(c) => c.pulse(button, delay_ms).await,
            PowerControlBackend::Software(c) => c.pulse(button, delay_ms).await,
        }
    }

    pub async fn verify(&mut self) -> Result<bool, PowerError> {
        match self {
            PowerControlBackend::Ch340(c) => c.verify().await,
            PowerControlBackend::External(c) => c.verify().await,
            PowerControlBackend::Software(c) => c.verify().await,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            PowerControlBackend::Ch340(c) => c.name(),
            PowerControlBackend::External(c) => c.name(),
            PowerControlBackend::Software(c) => c.name(),
        }
    }

    pub fn has_button(&self, button: Button) -> bool {
        match self {
            PowerControlBackend::Ch340(c) => c.has_button(button),
            PowerControlBackend::External(c) => c.has_button(button),
            PowerControlBackend::Software(c) => c.has_button(button),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_button_channels() {
        let ch = ButtonChannels {
            reset: 1,
            maskrom: 2,
            recovery: 0,
        };
        assert_eq!(ch.get(Button::Reset), 1);
        assert_eq!(ch.get(Button::Maskrom), 2);
        assert_eq!(ch.get(Button::Recovery), 0);
        assert!(ch.has_any());
    }

    #[test]
    fn test_button_channels_empty() {
        let ch = ButtonChannels::default();
        assert!(!ch.has_any());
        assert_eq!(ch.get(Button::Reset), 0);
    }

    #[test]
    fn test_ch340_configured() {
        let relay = Ch340RelayControl::new(
            "192.168.1.1".to_string(),
            2001,
            ButtonChannels {
                reset: 1,
                maskrom: 2,
                recovery: 0,
            },
        );
        assert!(relay.is_configured());
    }

    #[test]
    fn test_ch340_not_configured_zero_port() {
        let relay = Ch340RelayControl::new(
            "192.168.1.1".to_string(),
            0,
            ButtonChannels {
                reset: 1,
                maskrom: 0,
                recovery: 0,
            },
        );
        assert!(!relay.is_configured());
    }

    #[test]
    fn test_ch340_not_configured_no_channels() {
        let relay = Ch340RelayControl::new(
            "192.168.1.1".to_string(),
            2001,
            ButtonChannels::default(),
        );
        assert!(!relay.is_configured());
    }

    #[test]
    fn test_ch340_has_button() {
        let relay = Ch340RelayControl::new(
            "192.168.1.1".to_string(),
            2001,
            ButtonChannels {
                reset: 1,
                maskrom: 2,
                recovery: 0,
            },
        );
        assert!(relay.has_button(Button::Reset));
        assert!(relay.has_button(Button::Maskrom));
        assert!(!relay.has_button(Button::Recovery));
    }

    #[test]
    fn test_software_reboot() {
        let sw = SoftwareRebootControl::new();
        assert_eq!(sw.name(), "software-reboot");
        assert!(sw.has_button(Button::Reset));
        assert!(!sw.has_button(Button::Maskrom));
    }

    #[test]
    fn test_button_as_str() {
        assert_eq!(Button::Reset.as_str(), "reset");
        assert_eq!(Button::Maskrom.as_str(), "maskrom");
        assert_eq!(Button::Recovery.as_str(), "recovery");
    }

    #[tokio::test]
    async fn test_ch340_verify_not_configured() {
        let mut relay = Ch340RelayControl::new(
            "192.168.1.1".to_string(),
            0,
            ButtonChannels::default(),
        );
        let result = relay.verify().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_software_reboot_verify() {
        let mut sw = SoftwareRebootControl::new();
        let result = sw.verify().await;
        assert!(result.is_ok());
        assert!(!result.unwrap()); // false = not verifiable
    }

    #[tokio::test]
    async fn test_ch340_relay_with_mock_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 16];

            // Read ON packet
            let n = socket.read(&mut buf).await.unwrap();
            assert_eq!(n, 4);
            let _ = n;
            assert_eq!(buf[0], 0xA0);
            assert_eq!(buf[2], Ch340RelayControl::OP_ON);

            // Respond with status ON
            tokio::time::sleep(Duration::from_millis(10)).await;
            let status_resp = [0xA0, buf[1], 0x01, (0xA0 + buf[1] + 0x01) as u8];
            socket.write_all(&status_resp).await.unwrap();

            // Read STATUS packet
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = socket.read(&mut buf).await;

            // Respond with status ON
            let status_resp = [0xA0, 1, 0x01, (0xA0 + 1 + 0x01) as u8];
            socket.write_all(&status_resp).await.unwrap();

            // Read OFF packet
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _n = socket.read(&mut buf).await.unwrap();
            assert_eq!(buf[2], Ch340RelayControl::OP_OFF);

            // Respond with status OFF
            tokio::time::sleep(Duration::from_millis(10)).await;
            let status_resp = [0xA0, buf[1], 0x00, (0xA0 + buf[1] + 0x00) as u8];
            socket.write_all(&status_resp).await.unwrap();

            // Read STATUS packet
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = socket.read(&mut buf).await;

            // Respond with status OFF
            let status_resp = [0xA0, 1, 0x00, (0xA0 + 1 + 0x00) as u8];
            socket.write_all(&status_resp).await.unwrap();
        });

        let mut relay = Ch340RelayControl::new(
            "127.0.0.1".to_string(),
            port,
            ButtonChannels {
                reset: 1,
                maskrom: 0,
                recovery: 0,
            },
        );

        let result = relay.verify().await;
        assert!(result.is_ok());
        assert!(result.unwrap());

        server.await.unwrap();
    }
}
