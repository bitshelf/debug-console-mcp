//! Relay manager — TCP direct connection to ser2net controlling a CH340
//! relay, 4-byte protocol.
//!
//! Protocol (from serial_relay/src/main.rs):
//!   Packet: [0xA0, channel(1-4), opcode, checksum]
//!   checksum = (0xA0 + channel + opcode) & 0xFF
//!   Baud: 9600 8N1 (configured on the ser2net side)

use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

pub struct RelayManager {
    host: String,
    port: u16,
    reset_ch: u8,
    maskrom_ch: u8,
    recovery_ch: u8,
    stream: Option<TcpStream>,
    /// Handle to the most recent background reset task.
    /// Aborted on new reset_bg() or close() to prevent overlapping relay pulses.
    reset_handle: StdMutex<Option<JoinHandle<()>>>,
}

impl RelayManager {
    const HEADER: u8 = 0xA0;
    const OP_ON: u8 = 0x01;
    const OP_OFF: u8 = 0x00;
    const OP_STATUS: u8 = 0x05;

    pub fn new(
        host: String,
        port: u16,
        reset_channel: u8,
        maskrom_channel: u8,
        recovery_channel: u8,
    ) -> Self {
        Self {
            host,
            port,
            reset_ch: reset_channel,
            maskrom_ch: maskrom_channel,
            recovery_ch: recovery_channel,
            stream: None,
            reset_handle: StdMutex::new(None),
        }
    }

    /// Check if the relay is configured and the reset channel is valid (1-4).
    pub fn configured(&self) -> bool {
        self.port > 0 && self.reset_ch > 0 && self.reset_ch <= 4
    }

    /// Check if MASKROM mode is available (maskrom channel configured 1-4).
    pub fn maskrom_configured(&self) -> bool {
        self.configured() && self.maskrom_ch > 0 && self.maskrom_ch <= 4
    }

    pub fn maskrom_ch(&self) -> u8 {
        self.maskrom_ch
    }

    pub fn reset_ch(&self) -> u8 {
        self.reset_ch
    }

    pub fn recovery_ch(&self) -> u8 {
        self.recovery_ch
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Ensure the TCP connection is open (5s timeout).
    async fn ensure_open(&mut self) -> Result<(), std::io::Error> {
        if self.stream.is_none() {
            let addr = format!("{}:{}", self.host, self.port);
            let stream =
                tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr))
                    .await
                    .map_err(|_| {
                        std::io::Error::new(std::io::ErrorKind::TimedOut, "relay connect timeout")
                    })??;
            stream.set_nodelay(true).ok();
            self.stream = Some(stream);
        }
        Ok(())
    }

    /// Force-close and rebuild the connection.
    async fn force_reconnect(&mut self) -> Result<(), std::io::Error> {
        self.stream.take();
        self.ensure_open().await
    }

    pub fn close(&mut self) {
        self.stream.take();
        // Abort any in-flight background reset to prevent relay lockup.
        if let Some(handle) = self.reset_handle.lock().unwrap().take() {
            handle.abort();
        }
    }

    /// Send a 4-byte command packet (with auto-reconnect retry).
    /// Drains any relay response from the TCP buffer after sending to prevent
    /// stale data accumulating and causing backpressure on subsequent writes.
    async fn send_command(&mut self, channel: u8, opcode: u8) -> Result<Vec<u8>, std::io::Error> {
        let checksum = (Self::HEADER as u16 + channel as u16 + opcode as u16) & 0xFF;
        let packet = [Self::HEADER, channel, opcode, checksum as u8];

        for attempt in 0..2 {
            // Ensure connection
            if let Err(e) = self.ensure_open().await {
                if attempt == 0 {
                    self.stream.take();
                    continue;
                }
                return Err(e);
            }

            let stream = self.stream.as_mut().unwrap();

            // Send command
            match stream.write_all(&packet).await {
                Ok(_) => {}
                Err(_e) if attempt == 0 => {
                    self.force_reconnect().await?;
                    continue;
                }
                Err(e) => return Err(e),
            }
            stream.flush().await?;

            // Wait for relay hardware to process
            tokio::time::sleep(Duration::from_millis(150)).await;

            if opcode == Self::OP_STATUS {
                // STATUS query: read the response
                let mut buf = [0u8; 256];
                match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await
                {
                    Ok(Ok(n)) => return Ok(buf[..n].to_vec()),
                    Ok(Err(e)) => return Err(e),
                    Err(_) => return Ok(Vec::new()),
                }
            }

            // ON/OFF: drain any response the relay sends back to prevent
            // stale data from accumulating in the TCP kernel buffer.
            let mut drain_buf = [0u8; 256];
            let _ =
                tokio::time::timeout(Duration::from_millis(200), stream.read(&mut drain_buf)).await;
            return Ok(Vec::new());
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "relay send failed after retry",
        ))
    }

    async fn channel_on(&mut self, channel: u8) -> Result<(), std::io::Error> {
        self.send_command(channel, Self::OP_ON).await.map(|_| ())
    }

    async fn channel_off(&mut self, channel: u8) -> Result<(), std::io::Error> {
        self.send_command(channel, Self::OP_OFF).await.map(|_| ())
    }

    /// Pulse reset: RESET=low → 500ms → RESET=high.
    /// On failure, attempts to release the reset channel (rollback).
    pub async fn reset(&mut self) -> bool {
        if !self.configured() {
            return false;
        }
        match self.do_reset().await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("Relay reset failed: {e}, rolling back");
                let _ = self.channel_off(self.reset_ch).await;
                false
            }
        }
    }

    /// Non-blocking reset: spawns relay pulse in background, returns immediately.
    /// Read loop can capture boot data during the 500ms hold.
    ///
    /// Guarantees OFF is always sent — even on panic or early abort — via a
    /// Drop guard that sends the OFF packet as a last-resort cleanup.
    /// Tracks the task handle; subsequent calls abort the previous task first.
    pub fn reset_bg(&self) -> bool {
        if !self.configured() {
            return false;
        }

        // Abort previous reset task if still running to prevent overlapping pulses.
        {
            let mut guard = self.reset_handle.lock().unwrap();
            if let Some(prev) = guard.take() {
                prev.abort();
            }
        }

        let host = self.host.clone();
        let port = self.port;
        let ch = self.reset_ch;

        let handle = tokio::spawn(async move {
            let addr = format!("{host}:{port}");

            // Drop guard: sends OFF on any exit path (panic, early abort, etc.)
            struct ResetGuard {
                addr: String,
                ch: u8,
                off_sent: bool,
            }
            impl ResetGuard {
                async fn send_off(&mut self) {
                    if !self.off_sent {
                        self.off_sent = true;
                        if let Ok(mut s) = tokio::net::TcpStream::connect(&self.addr).await {
                            s.set_nodelay(true).ok();
                            let off = [
                                0xA0u8,
                                self.ch,
                                0x00,
                                (0xA0u16 + self.ch as u16) as u8 & 0xFF,
                            ];
                            let _ = tokio::io::AsyncWriteExt::write_all(&mut s, &off).await;
                            let _ = tokio::io::AsyncWriteExt::flush(&mut s).await;
                        }
                    }
                }
            }

            let mut guard = ResetGuard {
                addr,
                ch,
                off_sent: false,
            };

            // Connect and send ON
            let mut stream = match tokio::net::TcpStream::connect(&guard.addr).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("reset_bg: connect failed: {e}");
                    return; // Drop guard not needed (nothing was sent)
                }
            };
            stream.set_nodelay(true).ok();

            let on = [
                0xA0u8,
                ch,
                0x01,
                (0xA0u16 + ch as u16 + 1) as u8 & 0xFF,
            ];
            if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut stream, &on).await {
                tracing::warn!("reset_bg: ON write failed: {e}");
                return;
            }
            if let Err(e) = tokio::io::AsyncWriteExt::flush(&mut stream).await {
                tracing::warn!("reset_bg: ON flush failed: {e}");
                guard.send_off().await;
                return;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;

            // Send OFF normally
            guard.send_off().await;
        });

        // Track the handle
        *self.reset_handle.lock().unwrap() = Some(handle);
        true
    }

    async fn do_reset(&mut self) -> Result<(), std::io::Error> {
        self.channel_on(self.reset_ch).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.channel_off(self.reset_ch).await?;
        Ok(())
    }

    /// MASKROM sequence: MASKROM=low → RESET=low → RESET=high → MASKROM=high.
    /// Any step failure → rollback (release all pins).
    pub async fn enter_maskrom(&mut self) -> bool {
        if !self.maskrom_configured() {
            return false;
        }
        match self.do_maskrom().await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("Relay maskrom failed: {e}, rolling back");
                let _ = self.channel_off(self.reset_ch).await;
                let _ = self.channel_off(self.maskrom_ch).await;
                false
            }
        }
    }

    async fn do_maskrom(&mut self) -> Result<(), std::io::Error> {
        self.channel_on(self.maskrom_ch).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.channel_on(self.reset_ch).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.channel_off(self.reset_ch).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.channel_off(self.maskrom_ch).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configured() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0);
        assert!(relay.configured());
    }

    #[test]
    fn test_not_configured_zero_port() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 0, 2, 1, 0);
        assert!(!relay.configured());
    }

    #[test]
    fn test_not_configured_zero_channel() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 0, 1, 0);
        assert!(!relay.configured());
    }

    #[test]
    fn test_not_configured_channel_too_high() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 5, 1, 0);
        assert!(!relay.configured());
    }

    #[test]
    fn test_maskrom_not_configured_zero_channel() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 0, 0);
        assert!(!relay.maskrom_configured());
    }

    #[test]
    fn test_maskrom_not_configured_channel_too_high() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 5, 0);
        assert!(!relay.maskrom_configured());
    }

    #[test]
    fn test_maskrom_configured() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0);
        assert!(relay.maskrom_configured());
    }

    #[test]
    fn test_packet_checksum() {
        let header: u8 = 0xA0;
        let channel: u8 = 2;
        let opcode: u8 = RelayManager::OP_ON;
        let checksum = (header as u16 + channel as u16 + opcode as u16) & 0xFF;
        assert_eq!(checksum as u8, 0xA3);
    }

    #[test]
    fn test_packet_structure() {
        let header: u8 = RelayManager::HEADER;
        let channel: u8 = 1;
        let opcode: u8 = RelayManager::OP_OFF;
        let checksum = (header as u16 + channel as u16 + opcode as u16) & 0xFF;
        let packet = [header, channel, opcode, checksum as u8];

        assert_eq!(packet[0], 0xA0);
        assert_eq!(packet[1], 1);
        assert_eq!(packet[2], 0x00);
        assert_eq!(packet[3], (0xA0 + 1 + 0) as u8);
    }

    #[tokio::test]
    async fn test_relay_reset_not_configured() {
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 0, 0, 0, 0);
        let result = relay.reset().await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_relay_close() {
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0);
        relay.close();
    }

    #[tokio::test]
    async fn test_relay_reset_with_mock_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 16];

            // Read ON packet
            let n = socket.read(&mut buf).await.unwrap();
            assert_eq!(n, 4);
            assert_eq!(buf[0], 0xA0);
            assert_eq!(buf[2], RelayManager::OP_ON);

            tokio::time::sleep(Duration::from_millis(10)).await;

            // Read OFF packet
            let n = socket.read(&mut buf).await.unwrap();
            assert_eq!(n, 4);
            assert_eq!(buf[0], 0xA0);
            assert_eq!(buf[2], RelayManager::OP_OFF);
        });

        let mut relay = RelayManager::new("127.0.0.1".to_string(), port, 2, 1, 0);
        let result = relay.reset().await;
        assert!(result);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_relay_enter_maskrom_not_configured() {
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 0, 0);
        let result = relay.enter_maskrom().await;
        assert!(!result);
    }

    #[test]
    fn test_relay_manager_fields() {
        let relay = RelayManager::new("192.168.1.1".to_string(), 2001, 2, 1, 0);
        assert_eq!(relay.host, "192.168.1.1");
        assert_eq!(relay.port, 2001);
        assert_eq!(relay.reset_ch, 2);
        assert_eq!(relay.maskrom_ch, 1);
        assert!(relay.stream.is_none());
        assert!(relay.reset_handle.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_reset_bg_tracks_handle() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0);
        assert!(relay.reset_handle.lock().unwrap().is_none());

        // reset_bg spawns a task that will fail to connect (no server)
        assert!(relay.reset_bg());
        // Handle should now be set (even though task will fail)
        assert!(relay.reset_handle.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn test_reset_bg_aborts_previous() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0);

        // First reset_bg
        assert!(relay.reset_bg());
        let _handle1_finished = relay
            .reset_handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true); // Might already be done (connection failed)

        // Second reset_bg should abort the first
        assert!(relay.reset_bg());
        // The task should be a fresh spawn (not the same as handle1 if it was still running)
        assert!(relay.reset_handle.lock().unwrap().is_some());
    }

    #[test]
    fn test_reset_bg_not_configured_returns_false() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 0, 0, 0, 0);
        assert!(!relay.reset_bg());
        assert!(relay.reset_handle.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_relay_close_aborts_reset() {
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0);
        assert!(relay.reset_bg());
        assert!(relay.reset_handle.lock().unwrap().is_some());

        relay.close();
        assert!(relay.reset_handle.lock().unwrap().is_none());
    }

    #[test]
    fn test_channel_boundaries() {
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 1, 1, 0).configured());
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1, 0).configured());
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 3, 1, 0).configured());
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 4, 1, 0).configured());
        assert!(!RelayManager::new("127.0.0.1".to_string(), 2001, 0, 1, 0).configured());
        assert!(!RelayManager::new("127.0.0.1".to_string(), 2001, 5, 1, 0).configured());
    }
}
