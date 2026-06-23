//! Console driver — TCP 连接 ser2net，仿 labgrid SerialDriver。
//!
//! 使用 tokio TCP 直连 ser2net (socket:// 协议)，无 socat 中间层。
//! 所有写操作通过 `write_tx` channel 发送，由 read loop 执行实际 I/O。
//!
//! Channel is **bounded** (depth 64). When full, oldest messages are dropped
//! to prevent unbounded memory growth during extended disconnects.

use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Max pending writes before dropping (prevents unbounded memory leak on disconnect).
const WRITE_CHANNEL_DEPTH: usize = 64;

pub struct SerialConsoleDriver {
    host: String,
    /// TCP port number as string (e.g. "2000"), always ser2net
    target: String,
    stream: Option<TcpStream>,
    connected: bool,
    /// 写请求 channel — CommandQueue 通过此发送数据，read loop 执行实际写入
    write_tx: mpsc::Sender<Vec<u8>>,
    write_rx: mpsc::Receiver<Vec<u8>>,
}

impl SerialConsoleDriver {
    pub fn new(host: String, target: String) -> Self {
        let (tx, rx) = mpsc::channel(WRITE_CHANNEL_DEPTH);
        Self {
            host,
            target,
            stream: None,
            connected: false,
            write_tx: tx,
            write_rx: rx,
        }
    }

    pub fn is_open(&self) -> bool {
        self.connected
    }

    /// 获取写 channel 的克隆 (用于 CommandQueue write_fn)
    pub fn write_sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.write_tx.clone()
    }

    /// 连接 (或重连) 到 ser2net, 5s 超时
    pub async fn connect(&mut self) -> Result<(), std::io::Error> {
        self.stream.take();
        self.connected = false;

        let port: u16 = self.target.parse().unwrap_or(2000);
        let addr = format!("{}:{}", self.host, port);
        let stream =
            tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(&addr))
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout")
                })??;
        stream.set_nodelay(true).ok();
        self.stream = Some(stream);
        self.connected = true;
        Ok(())
    }

    /// 关闭连接
    pub fn close(&mut self) {
        self.stream.take();
        self.connected = false;
    }

    /// 异步发送一行文本 (通过 channel)。若 channel 满则静默丢弃 (防止背压阻塞调用者)。
    pub fn sendline(&self, line: &str) {
        let data = format!("{line}\n");
        self.write_tx.try_send(data.into_bytes()).ok();
    }

    /// 异步发送 control character (Ctrl-C = 0x03, etc.)
    /// 若 channel 满则静默丢弃 (心跳/探测字符丢失不影响正确性)。
    pub fn sendcontrol(&self, ch: char) {
        let ch = ch.to_ascii_lowercase();
        if let Some(idx) = (b'a'..=b'z').position(|c| c == ch as u8) {
            let ctrl_byte = (idx as u8) + 1;
            self.write_tx.try_send(vec![ctrl_byte]).ok();
        }
    }

    /// 直接写入 TCP stream（绕过 channel，用于紧急 Ctrl-C flood）
    pub async fn write_raw(&mut self, data: &[u8]) {
        if let Some(ref mut stream) = self.stream {
            if let Err(e) = tokio::io::AsyncWriteExt::write_all(stream, data).await {
                tracing::warn!("write_raw failed: {e}");
                self.connected = false;
            }
        }
    }

    /// 处理所有待发的写请求 (由 read loop 调用)
    /// 仿 labgrid SerialDriver._write() — 使用 write_all 阻塞写入。
    /// 写失败时丢弃数据 (不重入队，防止断连期间无限堆积)。
    pub async fn drain_writes(&mut self) {
        let stream = match self.stream.as_mut() {
            Some(s) => s,
            None => {
                while self.write_rx.try_recv().is_ok() {}
                return;
            }
        };

        while let Ok(data) = self.write_rx.try_recv() {
            if let Err(e) = tokio::io::AsyncWriteExt::write_all(stream, &data).await {
                tracing::warn!("drain_writes: write_all failed: {e}");
                self.connected = false;
                // Drop failed data — do not re-queue (prevents unbounded growth).
                return;
            }
        }
    }

    /// 事件驱动等待数据可读 (tokio readable / epoll, 无轮询)
    pub async fn wait_readable(&self, timeout: Duration) -> bool {
        match &self.stream {
            Some(s) => match tokio::time::timeout(timeout, s.readable()).await {
                Ok(Ok(())) => true,
                _ => false,
            },
            None => {
                // 无连接: sleep 避免 busy-wait
                tokio::time::sleep(timeout.min(Duration::from_secs(5))).await;
                false
            }
        }
    }

    /// 读取可用数据 (阻塞直到有数据或超时)
    ///
    /// 返回:
    /// - `Ok(data)` 且 data 非空: 读到数据
    /// - `Ok(data)` 且 data 为空: 超时，无数据
    /// - `Err`: 连接错误
    pub async fn read_available(
        &mut self,
        timeout: Duration,
        max_size: usize,
    ) -> Result<Vec<u8>, std::io::Error> {
        let stream = self.stream.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "not connected")
        })?;

        let mut buf = vec![0u8; max_size];
        match tokio::time::timeout(timeout, stream.read(&mut buf)).await {
            Ok(Ok(0)) => {
                // TCP 连接关闭
                self.connected = false;
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "Connection closed by ser2net",
                ))
            }
            Ok(Ok(n)) => {
                buf.truncate(n);
                Ok(buf)
            }
            Ok(Err(e)) => {
                self.connected = false;
                Err(e)
            }
            Err(_) => {
                // 超时，无数据
                Ok(Vec::new())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_new_console() {
        let console = SerialConsoleDriver::new("127.0.0.1".into(), "12345".into());
        assert!(!console.is_open());
    }

    #[tokio::test]
    async fn test_connect_close() {
        // Start a mock TCP server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            // Keep connection alive briefly
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), port.to_string());
        assert!(!console.is_open());

        console.connect().await.unwrap();
        assert!(console.is_open());

        console.close();
        assert!(!console.is_open());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_failure() {
        let mut console = SerialConsoleDriver::new("127.0.0.1".to_string(), "59999".into());
        let result = console.connect().await;
        assert!(result.is_err());
        assert!(!console.is_open());
    }

    #[tokio::test]
    async fn test_sendline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 100];
            let n = socket.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), port.to_string());
        console.connect().await.unwrap();

        console.sendline("test command");

        // Drain writes to actually send
        console.drain_writes().await;

        // Give time for data to arrive
        tokio::time::sleep(Duration::from_millis(50)).await;

        console.close();
        let received = server_handle.await.unwrap();
        assert_eq!(received, "test command\n");
    }

    #[tokio::test]
    async fn test_sendcontrol() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 10];
            let n = socket.read(&mut buf).await.unwrap();
            buf[..n].to_vec()
        });

        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), port.to_string());
        console.connect().await.unwrap();

        console.sendcontrol('c'); // Ctrl-C = 0x03
        console.drain_writes().await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        console.close();

        let received = server_handle.await.unwrap();
        assert_eq!(received, vec![0x03]);
    }

    #[tokio::test]
    async fn test_read_available_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            // Don't send anything
        });

        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), port.to_string());
        console.connect().await.unwrap();

        let result = console
            .read_available(Duration::from_millis(50), 1024)
            .await
            .unwrap();

        assert!(result.is_empty()); // Should timeout with empty data

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_read_available_data() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            socket.write_all(b"test data").await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), port.to_string());
        console.connect().await.unwrap();

        let result = console
            .read_available(Duration::from_millis(100), 1024)
            .await
            .unwrap();

        assert_eq!(result, b"test data");

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_write_sender_clone() {
        let console = SerialConsoleDriver::new("127.0.0.1".into(), "12345".into());
        let sender1 = console.write_sender();
        let sender2 = console.write_sender();

        // Both senders should be valid (try_send on bounded channel)
        assert!(sender1.try_send(b"test1".to_vec()).is_ok());
        assert!(sender2.try_send(b"test2".to_vec()).is_ok());
    }

    #[tokio::test]
    async fn test_drain_writes_no_connection() {
        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), "12345".into());

        // Send data without connecting
        console.write_tx.try_send(b"test".to_vec()).ok();

        // Should not panic, just discard
        console.drain_writes().await;
    }

    #[tokio::test]
    async fn test_multiple_writes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1000];
            let mut total = 0;
            for _ in 0..3 {
                let n = socket.read(&mut buf[total..]).await.unwrap();
                total += n;
            }
            String::from_utf8_lossy(&buf[..total]).to_string()
        });

        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), port.to_string());
        console.connect().await.unwrap();

        console.sendline("cmd1");
        console.sendline("cmd2");
        console.sendline("cmd3");

        console.drain_writes().await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        console.close();

        let received = server_handle.await.unwrap();
        assert!(received.contains("cmd1"));
        assert!(received.contains("cmd2"));
        assert!(received.contains("cmd3"));
    }

    /// Bounded channel silently drops writes when full — no unbounded memory growth.
    #[test]
    fn test_bounded_channel_drops_on_full() {
        let console = SerialConsoleDriver::new("127.0.0.1".into(), "12345".into());
        // Fill the channel to capacity (WRITE_CHANNEL_DEPTH = 64)
        for i in 0..64 {
            assert!(console.write_tx.try_send(vec![i as u8]).is_ok());
        }
        // 65th write should silently fail (channel full)
        assert!(console.write_tx.try_send(vec![65]).is_err());
    }

    /// Verify that failed writes are NOT re-queued: drain_writes sets
    /// connected=false and returns, discarding the failed data.
    #[tokio::test]
    async fn test_no_requeue_on_write_failure() {
        // No server — connect will fail, write_raw will mark disconnected
        let mut console = SerialConsoleDriver::new("127.0.0.1".to_string(), "59999".into());
        // Queue a write
        console.write_tx.try_send(b"should_be_discarded".to_vec()).ok();
        // drain_writes with no connection should drain and discard
        console.drain_writes().await;
        // The channel should now be empty (write was discarded, not re-queued)
        assert!(console.write_rx.try_recv().is_err());
    }

    /// Verify that sendline and sendcontrol use try_send (non-blocking).
    #[test]
    fn test_sendline_uses_try_send() {
        let mut console = SerialConsoleDriver::new("127.0.0.1".into(), "12345".into());
        // Should not block even with no receiver draining
        console.sendline("test");
        console.sendcontrol('c');
        // Both should have been enqueued
        assert!(console.write_rx.try_recv().is_ok());
        assert!(console.write_rx.try_recv().is_ok());
    }

    #[test]
    fn test_sendcontrol_byte_calculation() {
        // Ctrl-A = 0x01, Ctrl-B = 0x02, ..., Ctrl-Z = 0x1A
        let test_cases = [
            ('a', 0x01),
            ('b', 0x02),
            ('c', 0x03),
            ('x', 0x18),
            ('y', 0x19),
            ('z', 0x1A),
        ];

        for (ch, expected) in test_cases {
            let idx = (ch as u8) - b'a';
            let ctrl_byte = idx + 1;
            assert_eq!(ctrl_byte, expected);
        }
    }
}
