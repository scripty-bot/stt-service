//! Connection handler for STTS.

#[macro_use]
extern crate tracing;

use byteorder::ByteOrder;
use std::fmt::Write;
use std::time::Duration;
use stts_speech_to_text::{Error, get_load, SttStreamingState};
use systemstat::LoadAverage;
use tokio::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct ConnectionHandler {
    stream: TcpStream,
    stt_stream: Option<SttStreamingState>,
    verbose: bool,
}

impl From<TcpStream> for ConnectionHandler {
    fn from(stream: TcpStream) -> Self {
        Self {
            stream,
            stt_stream: None,
            verbose: false,
        }
    }
}

impl ConnectionHandler {
    /// Enter the main loop of the connection handler.
    pub async fn handle(&mut self) {
        loop {
            debug!("waiting for command");
            // read the type of the next incoming message
            let t: io::Result<u8> = tokio::select! {
                t = self.stream.read_u8() => t,
                _ = tokio::signal::ctrl_c() => {
                    let _ = self.stream.write_u8(0x05).await;
                    break;
                },
            };

            let t = match t {
                Ok(t) => t,
                Err(e) => {
                    error!("error reading message type: {}", e);
                    let _ = self.stream.write_u8(0xFD).await;
                    let _ = write_string(&mut self.stream, &e.to_string()).await;
                    break;
                }
            };
            debug!("received command: {:02x}", t);
            let res = match t {
                0x00 => self.handle_0x00().await,
                0x01 => self.handle_0x01().await,
                0x02 => self.handle_0x02().await,
                0x03 => self.handle_0x03().await,
                0x04 => self.handle_0x04().await,
                _ => {
                    warn!("unknown message type: {}", t);
                    let _ = self.stream.write_u8(0xFE).await.map(|_| false);
                    break;
                }
            };
            debug!("handled command: {:02x}", t);

            match res {
                Ok(e) if e => break,
                Err(e) => {
                    error!("error writing message: {}", e);
                    let _ = self.stream.write_u8(0xFD).await;
                    let _ = write_string(&mut self.stream, &e.to_string()).await;
                    break;
                }
                _ => {}
            };
        }
        debug!("exiting");
        // shutdown the connection
        if let Err(e) = self.stream.shutdown().await {
            error!("error shutting down connection: {}", e);
        }
    }

    async fn handle_0x00(&mut self) -> io::Result<bool> {
        // 0x00: Initialize Streaming

        // field 0: verbose: bool
        // read as a u8, then convert to bool
        trace!("reading verbose");
        let verbose = self.stream.read_u8().await? != 0;

        // field 1: language: String
        trace!("reading language");
        let _language = read_string(&mut self.stream).await?;

        debug!("loading stream");
        let stt_stream = SttStreamingState::new();
        debug!("loaded stream");

        self.stt_stream = Some(stt_stream);
        self.verbose = verbose;
        // success!
        self.stream.write_u8(0x00).await?;

        Ok(false)
    }

    async fn handle_0x01(&mut self) -> io::Result<bool> {
        // 0x01: Audio Data;

        // field 0: data_len: u32
        trace!("reading data length");
        let data_len = self.stream.read_u32().await?;
        trace!("need to read {} bytes", data_len);

        // field 1: data: Vec<i16>, of length data_len/2, in NetworkEndian order
        trace!("reading data");
        let mut buf = vec![0; data_len as usize];
        self.stream.read_exact(&mut buf).await?;

        // fetch the model, if it doesn't exist, return and ignore this message
        trace!("fetching model");
        let stream = match self.stt_stream {
            Some(ref model) => model,
            None => return Ok(false),
        };

        // if the model exists, *then* spend the handful of CPU cycles to process the audio data
        trace!("processing audio data");
        let mut data = vec![0; (data_len / 2) as usize];
        byteorder::NetworkEndian::read_i16_into(&buf, &mut data);
        trace!("found {} samples", data.len());

        debug!("feeding data");
        // feed the audio data to the model
        stream.feed_audio(data).await;

        Ok(false)
    }

    async fn handle_0x02(&mut self) -> io::Result<bool> {
        // 0x02: Finalize Streaming

        // no fields

        // fetch the model, if it doesn't exist, return and ignore this message
        trace!("fetching model");
        let stream = match self.stt_stream.take() {
            Some(model) => model,
            None => return Ok(false),
        };

        if self.verbose {
            debug!("finalizing model");
            match stream.finish_stream(true).await {
                Ok(r) => {
                    trace!("writing header");
                    self.stream.write_u8(0x03).await?;
                    trace!("writing num_transcripts");
                    self.stream.write_u32(1).await?;
                    trace!("writing transcript");
                    write_string(&mut self.stream, &r).await?;
                    trace!("writing confidence");
                    self.stream.write_f64(f64::NAN).await?;
                }
                Err(e) => {
                    warn!("error finalizing model: {:?}", e);
                    trace!("writing header");
                    self.stream.write_u8(0x04).await?;
                    trace!("writing error");
                    self.stream.write_i64(-1).await?;
                }
            }
        } else {
            debug!("finalizing model");
            match stream.finish_stream(false).await {
                Ok(s) => {
                    trace!("writing header");
                    self.stream.write_u8(0x02).await?;
                    trace!("writing transcript");
                    write_string(&mut self.stream, &s).await?;
                }
                Err(e) => {
                    warn!("error finalizing model: {:?}", e);
                    trace!("writing header");
                    self.stream.write_u8(0x04).await?;
                    trace!("writing error");
                    self.stream.write_i64(-1).await?;
                }
            }
        }

        Ok(true)
    }

    async fn handle_0x03(&mut self) -> io::Result<bool> {
        // 0x03: Close Connection

        // no fields

        // immediately close the connection
        Ok(true)
    }

    async fn handle_0x04(&mut self) -> io::Result<bool> {
        // 0x04: Convert to Status

        // no fields

        // send current status

        // grab the required settings from command line args
        let max_utilization = std::env::args()
            .nth(2)
            .and_then(|s| s.parse::<f64>().ok()).expect("max utilization should have been checked already")
            .min(1.0) * 4.0;
        info!("allowing max utilization of {}", max_utilization);

        let can_overload = std::env::args()
            .nth(3)
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(false);
        info!("can_overload: {}", can_overload);

        // send Status Connection Open (type: 0x06, fields: max_utilization: f64, can_overload: bool)
        trace!("writing header");
        self.stream.write_u8(0x06).await?;
        trace!("writing max_utilization");
        self.stream.write_f64(max_utilization).await?;
        trace!("writing can_overload");
        self.stream.write_u8(can_overload as u8).await?;

        loop {
            // wait for either 5 seconds, or for a message to be received
            let timeout = tokio::time::timeout(Duration::from_secs(5), self.stream.read_u8()).await;

            // check what we got
            match timeout {
                Ok(Ok(t)) if t == 0x03 => {
                    // 0x03: Close Connection
                    // close the connection
                    break Ok(true);
                }
                Ok(Err(e)) => {
                    // IO error: return it
                    return Err(e);
                }
                Err(_) => {
                    // timed out without a new message: send a Status Connection Data (type 0x07, fields: utilization: f64)

                    let utilization = get_load() as f64;

                    // send data
                    trace!("writing header");
                    self.stream.write_u8(0x07).await?;
                    trace!("writing utilization");
                    self.stream.write_f64(utilization).await?;
                    // done!
                }
                _ => {
                    // anything else is a no-op
                }
            }
        }
    }
}

async fn read_string(stream: &mut TcpStream) -> io::Result<String> {
    // strings are encoded as a u64 length followed by the string bytes
    let len = stream.read_u64().await?;
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

async fn write_string(stream: &mut TcpStream, string: &str) -> io::Result<()> {
    // strings are encoded as a u64 length followed by the string bytes
    // cache the bytes to prevent a second call to .as_bytes()
    let bytes = string.as_bytes();
    let len = bytes.len() as u64;
    stream.write_u64(len as u64).await?;
    stream.write_all(bytes).await?;
    Ok(())
}
