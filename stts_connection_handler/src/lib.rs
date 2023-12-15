#![feature(let_chains)]
//! Connection handler for STTS.

#[macro_use]
extern crate tracing;

use std::{
	fmt::{Debug, Display, Formatter},
	net::SocketAddr,
	sync::Arc,
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
					return false;
				} else {
					return true;
				}
			}
		}
	};
}

pub struct ConnectionHandler {
	internal_rx:  OwnedReadHalf,
	internal_tx:  Option<OwnedWriteHalf>,
	peer_address: SocketAddr,

	last_timeout:    Instant,
	stt_streams:     Arc<DashMap<Uuid, SttStreamingState>>,
	shutdown_handle: tokio::sync::broadcast::Receiver<()>,

	client_rx: Option<tokio::sync::mpsc::Receiver<ServerToClientMessage>>,
	client_tx: tokio::sync::mpsc::Sender<ServerToClientMessage>,
}

impl ConnectionHandler {
	pub fn new(
		internal_stream: TcpStream,
		peer_address: SocketAddr,
		stt_streams: Arc<DashMap<Uuid, SttStreamingState>>,
		shutdown_handle: tokio::sync::broadcast::Receiver<()>,
	) -> Self {
		let (internal_rx, internal_tx) = internal_stream.into_split();
		let (client_tx, client_rx) = tokio::sync::mpsc::channel(64);
		Self {
			internal_rx,
			internal_tx: Some(internal_tx),
			peer_address,
			shutdown_handle,
			client_rx: Some(client_rx),
			last_timeout: Instant::now(),
			stt_streams,
			client_tx,
		}
	}

	/// Enter the main loop of the connection handler.
	pub async fn handle(mut self) {
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

		if let Err(e) = Self::send_message(
			self.client_tx.clone(),
			ServerToClientMessage::StatusConnectionOpen(
				scripty_common::stt_transport_models::StatusConnectionOpen {
					max_utilization,
					can_overload,
				},
			),
		)
		.await
		{
			error!("error sending status message: {}", e);
			// we can try sending an error, but if that fails we should just give up
			if let Err(e) = Self::send_message(
				self.client_tx.clone(),
				ServerToClientMessage::FatalIoError(
					scripty_common::stt_transport_models::FatalIoError {
						error: e.to_string(),
					},
				),
			)
			.await
			{
				error!("error sending fatal error message: {}", e);
			}
			return; // don't continue if we can't send the status message
		}

		tokio::spawn(Self::send_message_loop(
			self.internal_tx
				.take()
				.expect("should only take internal_tx once"),
			self.client_rx
				.take()
				.expect("should only take client_rx once"),
		));

		loop {
			debug!("waiting for message");
			// read a message
			let msg = match self.read_message().await {
				Ok(msg) => msg,
				Err(e) => {
					error!("error reading message: {}", e);
					if e.is_recoverable() {
						continue;
					} else {
						break;
					}
				}
			};
			let msg = if let Some(msg) = msg {
				msg
			} else {
				// timed out, write another status message and update the timeout
				if let Err(e) = Self::send_message(
					self.client_tx.clone(),
					ServerToClientMessage::StatusConnectionData(
						scripty_common::stt_transport_models::StatusConnectionData {
							utilization: get_load() as f64,
						},
					),
				)
				.await && !e.is_recoverable()
				{
					break;
				}

				// check if we should shutdown
				if self.shutdown_handle.try_recv().is_ok() {
					break;
				} else {
					continue;
				}
			};

			tokio::spawn(Self::handle_message(
				msg,
				Arc::clone(&self.stt_streams),
				self.client_tx.clone(),
			));
		}
		info!("closing connection from {}", self.peer_address);
	}

	async fn handle_message(
		msg: ClientToServerMessage,
		stt_streams: Arc<DashMap<Uuid, SttStreamingState>>,
		client_tx: tokio::sync::mpsc::Sender<ServerToClientMessage>,
	) -> bool {
		match msg {
			ClientToServerMessage::InitializeStreaming(InitializeStreaming { id }) => {
				// create a new model
				let stream = SttStreamingState::new();
				// store the model
				stt_streams.insert(id, stream);
				// send success
				is_recoverable_handler!(
					Self::send_message(
						client_tx,
						ServerToClientMessage::InitializationComplete(InitializationComplete {
							id
						})
					)
					.await
				);
				false
			}
			ClientToServerMessage::AudioData(AudioData { data, id }) => {
				// feed the audio data to the model
				stt_streams.entry(id).or_default().feed_audio(data);
				// we don't need to send a response
				false
			}
			ClientToServerMessage::FinalizeStreaming(FinalizeStreaming {
				verbose,
				language,
				translate,
				id,
			}) => {
				let (id, stream) = match stt_streams.remove(&id) {
					Some(stream) => stream,
					None => {
						warn!("no model loaded for id {}", id);
						is_recoverable_handler!(
							Self::send_message(
								client_tx,
								ServerToClientMessage::SttError(SttError {
									id,
									error: "no model loaded for this id".to_string(),
								})
							)
							.await
						);
						return false; // ignore this message
					}
				};
				let final_result = match stream.finish_stream(language, verbose, translate).await {
					Ok(result) => ServerToClientMessage::SttResult(SttSuccess { id, result }),
					Err(e) => ServerToClientMessage::SttError(SttError {
						id,
						error: e.to_string(),
					}),
				};

				is_recoverable_handler!(Self::send_message(client_tx, final_result).await);
				false
			}
			ClientToServerMessage::CloseConnection => true,
		}
	}

	async fn send_message(
		client_tx: tokio::sync::mpsc::Sender<ServerToClientMessage>,
		msg: ServerToClientMessage,
	) -> Result<(), ConnectionHandlerError> {
		client_tx.send(msg).await.map_err(|e| {
			ConnectionHandlerError::Io(io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))
		})
	}

	async fn send_message_loop(
		mut tx: OwnedWriteHalf,
		mut rx: tokio::sync::mpsc::Receiver<ServerToClientMessage>,
	) {
		while let Some(msg) = rx.recv().await {
			if let Err(e) = Self::send_message_internal(&mut tx, msg).await {
				error!("error sending message: {}", e);
				break;
			}
		}

		// if we get here, the receiver was closed
		debug!("send_message_loop exited");
		if let Err(e) = tx.shutdown().await {
			error!("error shutting down socket: {}", e);
		}
	}

	async fn send_message_internal(
		tx: &mut OwnedWriteHalf,
		msg: ServerToClientMessage,
	) -> Result<(), ConnectionHandlerError> {
		debug!("sending message: {:?}", msg);

		// serialize the message with msgpack
		let mut payload = Vec::new();
		rmp_serde::encode::write(&mut payload, &msg)?;

		// prepare the payload size
		let payload_size = payload.len() as u64;
		let mut buf = [0; 8];
		byteorder::NetworkEndian::write_u64(&mut buf, payload_size);

		// write the magic bytes
		tx.write_all(&scripty_common::MAGIC_BYTES).await?;
		// write the payload size
		tx.write_all(&buf).await?;
		// write the payload
		tx.write_all(&payload).await?;
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
				self.last_timeout = Instant::now();
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
		let payload = rmp_serde::from_slice::<ClientToServerMessage>(&payload)?;
		debug!("received message: {:?}", payload);
		Ok(Some(payload))
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
