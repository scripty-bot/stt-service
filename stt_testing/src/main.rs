use std::{
	io,
	io::{Read, Write},
	net::{IpAddr, SocketAddr, TcpStream},
	time::Instant,
};

use byteorder::{ByteOrder, NetworkEndian, ReadBytesExt, WriteBytesExt};
use scripty_common::stt_transport_models::{
	AudioData,
	ClientToServerMessage,
	FinalizeStreaming,
	InitializeStreaming,
	ServerToClientMessage,
	StatusConnectionOpen,
};

fn main() {
	let test_file_path = std::env::args()
		.nth(1)
		.expect("first argument should be the path to the WAV file to test");
	let maybe_target_address = std::env::args().nth(2);

	let reader = hound::WavReader::open(test_file_path).expect("failed to open WAV file");
	// sample rate must be 16000 Hz
	// channels must be 1
	// sample format must be 16-bit signed integer
	let spec = reader.spec();
	assert_eq!(spec.sample_rate, 16000);
	assert_eq!(spec.channels, 1);
	assert_eq!(spec.sample_format, hound::SampleFormat::Int);
	assert_eq!(spec.bits_per_sample, 16);

	// checks passed? good
	// now we can read the WAV file's data
	let data = reader
		.into_samples::<i16>()
		.map(|x| x.expect("invalid sample"))
		.collect::<Vec<_>>();

	// open a TCP socket from the client to the server
	let target_addr: SocketAddr = maybe_target_address.map_or_else(
		|| (IpAddr::from([127, 0, 0, 1]), 7269).into(),
		|t| t.parse().expect("failed to parse target address"),
	);
	let mut socket = TcpStream::connect(target_addr).expect("failed to connect to server");

	// wait for the server to send a StatusConnectionOpen message
	println!("waiting for initialization");
	let message = read_socket_message(&mut socket);
	match message {
		ServerToClientMessage::StatusConnectionOpen(StatusConnectionOpen {
			max_utilization,
			can_overload,
		}) => {
			println!("max utilization: {}", max_utilization);
			println!("can overload: {}", can_overload);
		}
		_ => panic!("expected StatusConnectionOpen, got {:?}", message),
	};
	println!("server initialized");

	let id = uuid::Uuid::new_v4();

	// notify the server we want to start streaming
	println!("sending initialization message");
	let message = ClientToServerMessage::InitializeStreaming(InitializeStreaming { id });
	write_socket_message(&mut socket, &message);
	println!("initialization message sent");
	// wait for the server to send an InitializationComplete message
	println!("waiting for initialization complete");
	let message = read_socket_message(&mut socket);
	match message {
		ServerToClientMessage::InitializationComplete(_) => {}
		_ => panic!("expected InitializationComplete, got {:?}", message),
	};
	println!("initialization complete");

	// send the audio data to the server in chunks of 3,840 bytes (20ms, or 240 samples)
	for chunk in data.chunks(240) {
		println!("sending chunk");
		let message = ClientToServerMessage::AudioData(AudioData {
			data: chunk.to_vec(),
			id,
		});
		write_socket_message(&mut socket, &message);
		println!("chunk sent");
	}

	// send the finalize message
	println!("sending finalize message");
	let message = ClientToServerMessage::FinalizeStreaming(FinalizeStreaming {
		translate: false,
		verbose: false,
		language: "en".to_string(),
		id,
	});
	write_socket_message(&mut socket, &message);
	let st = Instant::now();
	println!("finalize message sent");

	// wait for the server to send a SttResult message
	println!("waiting for result");
	loop {
		let message = read_socket_message(&mut socket);
		match message {
			ServerToClientMessage::SttResult(result) => {
				println!("result: {}", result.result);
				println!("time: {:?}", st.elapsed());
				break;
			}
			_ => println!("expected SttResult, got {:?}", message),
		};
	}
	println!("result received");
}

fn read_socket_message(socket: &mut TcpStream) -> ServerToClientMessage {
	// read the magic bytes
	let mut magic = [0; 4];
	socket
		.read_exact(&mut magic)
		.expect("failed to read from socket");
	assert_eq!(magic, scripty_common::MAGIC_BYTES);

	// read the data length
	let mut data_length_bytes = [0; 8];
	socket
		.read_exact(&mut data_length_bytes)
		.expect("failed to read from socket");
	let data_length = NetworkEndian::read_u64(&data_length_bytes);

	// read the data
	let mut data = vec![0; data_length as usize];
	socket
		.read_exact(&mut data)
		.expect("failed to read from socket");

	// deserialize the data
	rmp_serde::from_slice(&data).expect("failed to deserialize data")
}

fn write_socket_message(socket: &mut TcpStream, message: &ClientToServerMessage) {
	// serialize the message
	let mut data = Vec::new();
	rmp_serde::encode::write(&mut data, message).expect("failed to serialize message");

	// write the magic bytes
	socket
		.write_all(&scripty_common::MAGIC_BYTES)
		.expect("failed to write to socket");

	// write the data length
	socket
		.write_u64::<NetworkEndian>(data.len() as u64)
		.expect("failed to write to socket");

	// write the data
	socket.write_all(&data).expect("failed to write to socket");

	// flush the socket
	socket.flush().expect("failed to flush socket");
}
