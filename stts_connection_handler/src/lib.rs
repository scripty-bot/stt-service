//! Connection handler for STTS.

#[macro_use]
extern crate tracing;

use byteorder::ByteOrder;
use std::fmt::Write;
use stts_speech_to_text::{Error, Stream};
use tokio::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct ConnectionHandler {
    stream: UnixStream,
    model: Option<Stream>,
    verbose: bool,
}

impl From<UnixStream> for ConnectionHandler {
    fn from(stream: UnixStream) -> Self {
        Self {
            stream,
            model: None,
            verbose: false,
        }
    }
}

impl ConnectionHandler {
    /// Enter the main loop of the connection handler.
    pub async fn handle(&mut self) {
        loop {
            // read the type of the next incoming message
            let t = match self.stream.read_u8().await {
                Ok(t) => t,
                Err(e) => {
                    error!("error reading message type: {}", e);
                    let _ = self.stream.write_u8(0xFD).await;
                    let _ = write_string(&mut self.stream, &e.to_string()).await;
                    break;
                }
            };
            let res = match t {
                0x00 => self.handle_0x00().await,
                0x01 => self.handle_0x01().await,
                0x02 => self.handle_0x02().await,
                _ => {
                    info!("unknown message type: {}", t);
                    let _ = self.stream.write_u8(0xFE).await.map(|_| false);
                    break;
                }
            };

            let exit;
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
        if let Err(e) = self.stream.shutdown().await {
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

        let retval =
            match tokio::task::spawn_blocking(move || stts_speech_to_text::get_stream(&language))
                .await
                .ok()
                .flatten()
            {
                Some(stream) => {
                    self.model = Some(stream);
                    self.verbose = verbose;
                    // success!
                    self.stream.write_u8(0x00).await?;
                    false
                }
                None => {
                    // error!
                    self.stream.write_u8(0xFE).await?;
                    true
                }
            };
        Ok(retval)
    }

    async fn handle_0x01(&mut self) -> io::Result<bool> {
        // 0x01: Audio Data;

        // field 0: data_len: u32
        let data_len = self.stream.read_u32().await?;

        // field 1: data: Vec<i16>, of length data_len/2, in NetworkEndian order
        let mut buf = vec![0; data_len as usize];
        self.stream.read_exact(&mut buf).await?;

        // fetch the model, if it doesn't exist, return and ignore this message
        let model = match self.model {
            Some(ref mut model) => model,
            None => return Ok(false),
        };

        // if the model exists, *then* spend the handful of CPU cycles to process the audio data
        let mut data = vec![0; (data_len / 2) as usize];
        byteorder::NetworkEndian::read_i16_into(&buf, &mut data);

        // feed the audio data to the model
        tokio::task::block_in_place(|| {
            model.feed_audio(&data);
        });

        Ok(false)
    }

    async fn handle_0x02(&mut self) -> io::Result<bool> {
        // 0x02: Finalize Streaming

        // no fields

        // fetch the model, if it doesn't exist, return and ignore this message
        let model = match self.model.take() {
            Some(model) => model,
            None => return Ok(false),
        };

        if self.verbose {
            match model.finish_stream_with_metadata(3) {
                Ok(r) => {
                    self.stream.write_u8(0x03).await?;
                    let num = r.num_transcripts();
                    self.stream.write_u32(num).await?;
                    if num != 0 {
                        let transcripts = r.transcripts();
                        let main_transcript = unsafe { transcripts.get_unchecked(0) };
                        let tokens = main_transcript.tokens();
                        let mut res = String::new();
                        for token in tokens {
                            res.write_str(token.text().as_ref())
                                .expect("error writing to string");
                        }
                        write_string(&mut self.stream, &res).await?;
                        self.stream.write_f64(main_transcript.confidence()).await?;
                    }
                }
                Err(e) => {
                    self.stream.write_u8(0x04).await?;
                    let num_err = conv_err(e);
                    self.stream.write_i64(num_err).await?;
                }
            }
        } else {
            match model.finish_stream() {
                Ok(s) => {
                    self.stream.write_u8(0x02).await?;
                    write_string(&mut self.stream, &s).await?;
                }
                Err(e) => {
                    self.stream.write_u8(0x04).await?;
                    let num_err = conv_err(e);
                    self.stream.write_i64(num_err).await?;
                }
            }
        }

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

fn conv_err(e: Error) -> i64 {
    match e {
        Error::NoModel => 2147483649,
        Error::InvalidAlphabet => 0x2000,
        Error::InvalidShape => 0x2001,
        Error::InvalidScorer => 0x2002,
        Error::ModelIncompatible => 0x2003,
        Error::ScorerNotEnabled => 0x2004,
        Error::ScorerUnreadable => 0x2005,
        Error::ScorerInvalidHeader => 0x2006,
        Error::ScorerNoTrie => 0x2007,
        Error::ScorerInvalidTrie => 0x2008,
        Error::ScorerVersionMismatch => 0x2009,
        Error::InitMmapFailed => 0x3000,
        Error::InitSessionFailed => 0x3001,
        Error::InterpreterFailed => 0x3002,
        Error::RunSessionFailed => 0x3003,
        Error::CreateStreamFailed => 0x3004,
        Error::ReadProtoBufFailed => 0x3005,
        Error::CreateSessionFailed => 0x3006,
        Error::CreateModelFailed => 0x3007,
        Error::InsertHotWordFailed => 0x3008,
        Error::ClearHotWordsFailed => 0x3009,
        Error::EraseHotWordFailed => 0x3010,
        Error::Other(n) => n as i64,
        Error::Unknown => 2147483650,
        Error::NulBytesFound => 2147483651,
        Error::Utf8Error(_) => 2147483652,
        _ => i64::MIN,
    }
}
