//! Connection handler for STTS.

#[macro_use]
extern crate tracing;

use std::{
	fmt::{Debug, Display, Formatter},
	net::SocketAddr,
	time::Duration,
};

use byteorder::ByteOrder;
use dashmap::DashMap;
use scripty_common::stt_transport_models::{
	AudioData,
	ClientToServerMessage,
	FinalizeStreaming,
	InitializationComplete,
	InitializeStreaming,
	ServerToClientMessage,
	SttError,
	SttSuccess,
};
use stts_speech_to_text::{get_load, SttStreamingState};
use tokio::{
	io,
	io::{AsyncReadExt, AsyncWriteExt},
	net::{
		tcp::{OwnedReadHalf, OwnedWriteHalf},
		TcpStream,
	},
	time::Instant,
};
use uuid::Uuid;

macro_rules! is_recoverable_handler {
	($result: expr) => {
		match $result {
			Ok(res) => res,
			Err(e) => {
				error!("error reading message: {}", e);
				if e.is_recoverable() {
					continue;
				} else {
					break;
				}
			}
		}
	};
}

pub struct ConnectionHandler {
	internal_rx:     OwnedReadHalf,
	internal_tx:     OwnedWriteHalf,
	last_timeout:    Instant,
	peer_address:    SocketAddr,
	stt_streams:     DashMap<Uuid, SttStreamingState>,
	shutdown_handle: tokio::sync::broadcast::Receiver<()>,
}

impl ConnectionHandler {
	pub fn new(
		internal_stream: TcpStream,
		peer_address: SocketAddr,
		shutdown_handle: tokio::sync::broadcast::Receiver<()>,
	) -> Self {
		let (internal_rx, internal_tx) = internal_stream.into_split();
		Self {
			internal_rx,
			internal_tx,
			peer_address,
			shutdown_handle,
			last_timeout: Instant::now(),
			stt_streams: DashMap::new(),
		}
	}

	/// Enter the main loop of the connection handler.
	pub async fn handle(&mut self) {
		// write the initial status message
		let max_utilization = std::env::args()
			.nth(2)
			.and_then(|s| s.parse::<f64>().ok())
			.expect("max utilization should have been checked already")
			.min(1.0) * 4.0;
		info!("allowing max utilization of {}", max_utilization);

		let can_overload = std::env::args()
			.nth(3)
			.and_then(|s| s.parse::<bool>().ok())
			.unwrap_or(false);
		info!("can_overload: {}", can_overload);

		if let Err(e) = self
			.send_message(ServerToClientMessage::StatusConnectionOpen(
				scripty_common::stt_transport_models::StatusConnectionOpen {
					max_utilization,
					can_overload,
				},
			))
			.await
		{
			error!("error sending status message: {}", e);
			// we can try sending an error, but if that fails we should just give up
			if let Err(e) = self
				.send_message(ServerToClientMessage::FatalIoError(
					scripty_common::stt_transport_models::FatalIoError {
						error: e.to_string(),
					},
				))
				.await
			{
				error!("error sending fatal error message: {}", e);
			}
			return; // don't continue if we can't send the status message
		}

		loop {
			debug!("waiting for message");
			// read a message
			let msg = if let Some(msg) = is_recoverable_handler!(self.read_message().await) {
				msg
			} else {
				// timed out, write another status message and update the timeout
				is_recoverable_handler!(
					self.send_message(ServerToClientMessage::StatusConnectionData(
						scripty_common::stt_transport_models::StatusConnectionData {
							utilization: get_load() as f64,
						}
					))
					.await
				);
				self.last_timeout = Instant::now();

				// check if we should shutdown
				if self.shutdown_handle.try_recv().is_ok() {
					break;
				} else {
					continue;
				}
			};

			match msg {
				ClientToServerMessage::InitializeStreaming(InitializeStreaming {
					verbose,
					language,
					id,
				}) => {
					// create a new model
					let stream = SttStreamingState::new(language, verbose);
					// store the model
					self.stt_streams.insert(id, stream);
					// send success
					is_recoverable_handler!(
						self.send_message(ServerToClientMessage::InitializationComplete(
							InitializationComplete { id }
						))
						.await
					);
				}
				ClientToServerMessage::AudioData(AudioData { data, id }) => {
					// fetch the model

					// we have to check this first because we cannot send a message in the None
					// case due to the borrow checker (&mut self + &self)
					if !self.stt_streams.contains_key(&id) {
						warn!("no model loaded for id {}", id);
						is_recoverable_handler!(
							self.send_message(ServerToClientMessage::SttError(SttError {
								id,
								error: "no model loaded for this id".to_string(),
							}))
							.await
						);
						continue; // just ignore this message
					}

					let stream = match self.stt_streams.get(&id) {
						Some(stream) => stream,
						None => {
							unreachable!("already asserted that the key exists, but it doesn't")
						}
					};
					// feed the audio data to the model
					stream.feed_audio(data).await;
					// we don't need to send a response
				}
				ClientToServerMessage::FinalizeStreaming(FinalizeStreaming { id }) => {
					let (id, stream) = match self.stt_streams.remove(&id) {
						Some(stream) => stream,
						None => {
							warn!("no model loaded for id {}", id);
							is_recoverable_handler!(
								self.send_message(ServerToClientMessage::SttError(SttError {
									id,
									error: "no model loaded for this id".to_string(),
								}))
								.await
							);
							continue; // ignore this message
						}
					};
					let final_result = match stream.finish_stream().await {
						Ok(result) => ServerToClientMessage::SttResult(SttSuccess { id, result }),
						Err(e) => ServerToClientMessage::SttError(SttError {
							id,
							error: e.to_string(),
						}),
					};

					is_recoverable_handler!(self.send_message(final_result).await);
				}
				ClientToServerMessage::CloseConnection => break,
			}
		}
		info!("closing connection from {}", self.peer_address);
		// shutdown the connection
		if let Err(e) = self.internal_tx.shutdown().await {
			error!(
				"error shutting down connection from {}: {}",
				self.peer_address, e
			);
		}
	}

	async fn send_message(
		&mut self,
		msg: ServerToClientMessage,
	) -> Result<(), ConnectionHandlerError> {
		// serialize the message with msgpack
		let mut payload = Vec::new();
		rmp_serde::encode::write(&mut payload, &msg)?;

		// prepare the payload size
		let payload_size = payload.len() as u64;
		let mut buf = [0; 8];
		byteorder::NetworkEndian::write_u64(&mut buf, payload_size);

		// write the magic bytes
		self.internal_tx
			.write_all(&scripty_common::MAGIC_BYTES)
			.await?;
		// write the payload size
		self.internal_tx.write_all(&buf).await?;
		// write the payload
		self.internal_tx.write_all(&payload).await?;
		Ok(())
	}

	/// Read a message from the connection.
	///
	/// Returns `Ok(None)` if the five-second timeout is reached.
	async fn read_message(
		&mut self,
	) -> Result<Option<ClientToServerMessage>, ConnectionHandlerError> {
		// read 4 magic bytes
		let mut magic = [0; 4];

		// start by reading only one byte with a timeout of 5 seconds
		match tokio::time::timeout_at(
			self.last_timeout + Duration::from_secs(5),
			self.internal_rx.read_exact(&mut magic[0..1]),
		)
		.await
		{
			Ok(Ok(num)) => {
				if num != 1 {
					// if we didn't get one byte, return an error
					return Err(ConnectionHandlerError::Io(io::Error::new(
						io::ErrorKind::UnexpectedEof,
						"failed to read magic bytes",
					)));
				}
				// if we got the first byte, read the rest
				self.internal_rx.read_exact(&mut magic[1..]).await?;
			}
			Ok(Err(e)) => {
				// if we got an error, return it
				return Err(e.into());
			}
			Err(_) => {
				// if we timed out, return Ok(None)
				return Ok(None);
			}
		};

		// check the magic bytes
		if magic != scripty_common::MAGIC_BYTES {
			warn!("invalid magic bytes: {:?}", magic);
			return Err(ConnectionHandlerError::InvalidMagicBytes(magic));
		}

		// read the payload size as a u64
		let mut buf = [0; 8];
		self.internal_rx.read_exact(&mut buf).await?;
		let payload_size = byteorder::NetworkEndian::read_u64(&buf);

		// read payload size bytes
		let mut payload = vec![0; payload_size as usize];
		self.internal_rx.read_exact(&mut payload).await?;

		// deserialize the payload with msgpack
		Ok(Some(rmp_serde::from_slice::<ClientToServerMessage>(
			&payload,
		)?))
	}
}

#[derive(Debug)]
pub enum ConnectionHandlerError {
	Io(io::Error),
	MsgPackDecode(rmp_serde::decode::Error),
	MsgPackEncode(rmp_serde::encode::Error),
	InvalidMagicBytes([u8; 4]),
}

impl Display for ConnectionHandlerError {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		match self {
			ConnectionHandlerError::Io(e) => write!(f, "IO error: {}", e),
			ConnectionHandlerError::MsgPackDecode(e) => write!(f, "MsgPack decode error: {}", e),
			ConnectionHandlerError::MsgPackEncode(e) => write!(f, "MsgPack encode error: {}", e),
			ConnectionHandlerError::InvalidMagicBytes(bytes) => {
				write!(f, "invalid magic bytes: {:?}", bytes)
			}
		}
	}
}

impl std::error::Error for ConnectionHandlerError {}

impl ConnectionHandlerError {
	fn is_recoverable(&self) -> bool {
		match self {
			ConnectionHandlerError::Io(_) => false,
			ConnectionHandlerError::MsgPackDecode(_) => true,
			ConnectionHandlerError::MsgPackEncode(_) => true,
			ConnectionHandlerError::InvalidMagicBytes(_) => false,
		}
	}
}

impl From<io::Error> for ConnectionHandlerError {
	fn from(e: io::Error) -> Self {
		ConnectionHandlerError::Io(e)
	}
}

impl From<rmp_serde::decode::Error> for ConnectionHandlerError {
	fn from(e: rmp_serde::decode::Error) -> Self {
		ConnectionHandlerError::MsgPackDecode(e)
	}
}

impl From<rmp_serde::encode::Error> for ConnectionHandlerError {
	fn from(e: rmp_serde::encode::Error) -> Self {
		ConnectionHandlerError::MsgPackEncode(e)
	}
}
