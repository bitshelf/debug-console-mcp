//! Relay manager — TCP 直连 ser2net 控制 CH340 继电器，4 字节协议。
//!
//! 来自 serial_relay/src/main.rs 协议:
//!   Packet: [0xA0, channel(1-4), opcode, checksum]
//!   checksum = (0xA0 + channel + opcode) & 0xFF
//!   Baud: 9600 8N1 (ser2net 端已配置)

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[allow(dead_code)]
pub struct RelayManager {
    host: String,
    port: u16,
    reset_ch: u8,
    maskrom_ch: u8,
    stream: Option<TcpStream>,
}

impl RelayManager {
    const HEADER: u8 = 0xA0;
    const OP_ON: u8 = 0x01;
    const OP_OFF: u8 = 0x00;
    #[allow(dead_code)]
    const OP_TOGGLE: u8 = 0x04;
    const OP_STATUS: u8 = 0x05;

    pub fn new(host: String, port: u16, reset_channel: u8, maskrom_channel: u8) -> Self {
        Self {
            host,
            port,
            reset_ch: reset_channel,
            maskrom_ch: maskrom_channel,
            stream: None,
        }
    }

    pub fn configured(&self) -> bool {
        self.port > 0 && self.reset_ch > 0 && self.reset_ch <= 4
    }

    /// 确保 TCP 连接已打开 (5s 超时)
    async fn ensure_open(&mut self) -> Result<(), std::io::Error> {
        let need_connect = match &self.stream {
            None => true,
            Some(_) => false,
        };
        if need_connect {
            let addr = format!("{}:{}", self.host, self.port);
            let stream = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                TcpStream::connect(&addr)
            ).await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "relay connect timeout"))?
                ?;
            stream.set_nodelay(true).ok();
            self.stream = Some(stream);
        }
        Ok(())
    }

    /// 强制关闭并重建连接
    async fn force_reconnect(&mut self) -> Result<(), std::io::Error> {
        self.stream.take();
        self.ensure_open().await
    }

    pub fn close(&mut self) {
        self.stream.take();
    }

    /// 发送 4 字节命令包 (带自动重连重试)
    async fn send_command(&mut self, channel: u8, opcode: u8) -> Result<Vec<u8>, std::io::Error> {
        let checksum = (Self::HEADER as u16 + channel as u16 + opcode as u16) & 0xFF;
        let packet = [Self::HEADER, channel, opcode, checksum as u8];

        for attempt in 0..2 {
            // 确保连接
            if let Err(e) = self.ensure_open().await {
                if attempt == 0 {
                    self.stream.take();
                    continue;
                }
                return Err(e);
            }

            let stream = self.stream.as_mut().unwrap();

            // 清空输入缓冲 + 发送命令
            match stream.write_all(&packet).await {
                Ok(_) => {}
                Err(_e) if attempt == 0 => {
                    self.force_reconnect().await?;
                    continue;
                }
                Err(e) => return Err(e),
            }
            stream.flush().await?;

            // 等待继电器响应
            tokio::time::sleep(Duration::from_millis(50)).await;

            if opcode == Self::OP_STATUS {
                let mut buf = [0u8; 16];
                match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await
                {
                    Ok(Ok(n)) => return Ok(buf[..n].to_vec()),
                    Ok(Err(e)) => return Err(e),
                    Err(_) => return Ok(Vec::new()),
                }
            }
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

    /// 脉冲复位: RESET=低 → 500ms → RESET=高
    pub async fn reset(&mut self) -> bool {
        if !self.configured() {
            return false;
        }
        match self.do_reset().await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("Relay reset failed: {e}");
                false
            }
        }
    }

    async fn do_reset(&mut self) -> Result<(), std::io::Error> {
        self.channel_on(self.reset_ch).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.channel_off(self.reset_ch).await?;
        Ok(())
    }

    /// MASKROM 序列: MASKROM=低 → RESET=低 → RESET=高 → MASKROM=高
    /// 任何步骤失败 → 回滚释放所有引脚
    #[allow(dead_code)]
    pub async fn enter_maskrom(&mut self) -> bool {
        if !self.configured() || self.maskrom_ch == 0 {
            return false;
        }
        match self.do_maskrom().await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("Relay maskrom failed: {e}, rolling back");
                // 回滚: 确保所有引脚释放
                let _ = self.channel_off(self.reset_ch).await;
                let _ = self.channel_off(self.maskrom_ch).await;
                false
            }
        }
    }

    #[allow(dead_code)]
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
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1);
        assert!(relay.configured());
    }

    #[test]
    fn test_not_configured_zero_port() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 0, 2, 1);
        assert!(!relay.configured());
    }

    #[test]
    fn test_not_configured_zero_channel() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 0, 1);
        assert!(!relay.configured());
    }

    #[test]
    fn test_not_configured_channel_too_high() {
        let relay = RelayManager::new("127.0.0.1".to_string(), 2001, 5, 1);
        assert!(!relay.configured());
    }

    #[test]
    fn test_packet_checksum() {
        // Header = 0xA0, channel = 2, opcode ON = 0x01
        // checksum = (0xA0 + 2 + 1) & 0xFF = 0xA3
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
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 0, 0, 0);
        let result = relay.reset().await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_relay_close() {
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1);
        // Close without connecting should not panic
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
            assert_eq!(buf[0], 0xA0); // header
            assert_eq!(buf[2], RelayManager::OP_ON);

            tokio::time::sleep(Duration::from_millis(10)).await;

            // Read OFF packet
            let n = socket.read(&mut buf).await.unwrap();
            assert_eq!(n, 4);
            assert_eq!(buf[0], 0xA0);
            assert_eq!(buf[2], RelayManager::OP_OFF);
        });

        let mut relay = RelayManager::new("127.0.0.1".to_string(), port, 2, 1);
        let result = relay.reset().await;
        assert!(result);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_relay_enter_maskrom_not_configured() {
        let mut relay = RelayManager::new("127.0.0.1".to_string(), 2001, 2, 0);
        let result = relay.enter_maskrom().await;
        assert!(!result);
    }

    #[test]
    fn test_relay_manager_fields() {
        let relay = RelayManager::new("192.168.1.1".to_string(), 2001, 2, 1);
        assert_eq!(relay.host, "192.168.1.1");
        assert_eq!(relay.port, 2001);
        assert_eq!(relay.reset_ch, 2);
        assert_eq!(relay.maskrom_ch, 1);
        assert!(relay.stream.is_none());
    }

    #[test]
    fn test_channel_boundaries() {
        // Valid channels: 1-4
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 1, 1).configured());
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 2, 1).configured());
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 3, 1).configured());
        assert!(RelayManager::new("127.0.0.1".to_string(), 2001, 4, 1).configured());
        assert!(!RelayManager::new("127.0.0.1".to_string(), 2001, 0, 1).configured());
        assert!(!RelayManager::new("127.0.0.1".to_string(), 2001, 5, 1).configured());
    }
}
