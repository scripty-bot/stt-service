//! Connection handler for STTS.

#[macro_use]
extern crate tracing;

use tokio::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct ConnectionHandler {
    stream: UnixStream,
}

impl From<UnixStream> for ConnectionHandler {
    fn from(stream: UnixStream) -> Self {
        Self { stream }
    }
}

impl ConnectionHandler {
    /// Enter the main loop of the connection handler.
    pub async fn handle(&mut self) {
        loop {
            // read the type of the next incoming message
            let t = match stream.read_u8().await {
                Ok(t) => t,
                Err(e) => {
                    error!("error reading message type: {}", e);
                    let _ = self.stream.write_u8(0xFF).await;
                    break;
                }
            };
            let mut exit = bool;

            let res = match t {
                0x00 => self.handle_0x00().await,
                0x01 => self.handle_0x01().await,
                0x02 => self.handle_0x02().await,
                _ => {
                    info!("unknown message type: {}", t);
                    exit = true;
                    self.stream.write_u8(0xFE).await.map(|_| false)
                }
            };
            match res {
                Ok(e) => exit = e,
                Err(e) => {
                    error!("error writing message: {}", e);
                    exit = true;
                    let _ = self.stream.write_u8(0xFD).await;
                    let _ = write_string(&mut self.stream, &e.to_string()).await;
                }
            };

            if exit {
                break;
            }
        }
        // shutdown the connection
        if let Err(e) = stream.shutdown() {
            error!("error shutting down connection: {}", e);
        }
    }

    async fn handle_0x00(&mut self) -> io::Result<bool> {
        // 0x00: Initialize Streaming

        // field 0: verbose: bool
        // read as a u8, then convert to bool
        let verbose = self.stream.read_u8().await? != 0;

        // field 1: language: String
        let language = read_string(&mut self.stream).await?;

        // TODO: send message to backend to actually initialize streaming

        // send acknowledgement
        self.stream.write_u8(0x00).await?;

        Ok(false)
    }

    async fn handle_0x01(&mut self) -> io::Result<bool> {
        // 0x01: Audio Data

        // field 0: channels: u8
        let channels = self.stream.read_u8().await?;

        // field 1: rate: u32
        let rate = self.stream.read_u32().await?;

        // field 2: data_len: u32
        let data_len = self.stream.read_u32().await?;

        // field 3: data: Vec<u8>, of length data_len
        let mut buf = vec![0; data_len as usize];
        let data = self.stream.read_exact(&mut buf).await?;

        // TODO: send message to backend to actually process the audio data

        Ok(false)
    }

    async fn handle_0x02(&mut self) -> io::Result<bool> {
        // 0x02: Finalize Streaming

        // no fields

        // TODO: send message to backend to actually finalize streaming

        // TODO: return result of backend processing

        Ok(true)
    }
}

async fn read_string(stream: &mut UnixStream) -> io::Result<String> {
    // strings are encoded as a u64 length followed by the string bytes
    let len = stream.read_u64().await?;
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

async fn write_string(stream: &mut UnixStream, string: &str) -> io::Result<()> {
    // strings are encoded as a u64 length followed by the string bytes
    // cache the bytes to prevent a second call to .as_bytes()
    let bytes = string.as_bytes();
    let len = bytes.len() as u64;
    stream.write_u64(len as u64).await?;
    stream.write_all(bytes).await?;
    Ok(())
}
