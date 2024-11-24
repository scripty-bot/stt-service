use std::{
	fs::OpenOptions,
	net::{IpAddr, SocketAddr},
	sync::Arc,
	time::SystemTime,
};

use dashmap::DashMap;
use fenrir_rs::{NetworkingBackend, SerializationFormat};
use fern::{
	colors::{Color, ColoredLevelConfig},
	Dispatch,
};
use stts_speech_to_text::SttStreamingState;
use tokio::net::TcpStream;
use url::Url;
use uuid::Uuid;

#[macro_use]
extern crate tracing;

fn main() {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.worker_threads(4)
		.build()
		.unwrap()
		.block_on(main_inner());
}

fn init_logging() {
	// configure colors for the whole line
	let colors_line = ColoredLevelConfig::new()
		.error(Color::Red)
		.warn(Color::Yellow)
		// we actually don't need to specify the color for debug and info, they are white by default
		.info(Color::White)
		.debug(Color::White)
		// depending on the terminals color scheme, this is the same as the background color
		.trace(Color::BrightBlack);

	// configure colors for the name of the level.
	// since almost all of them are the same as the color for the whole line, we
	// just clone `colors_line` and overwrite our changes
	let colors_level = colors_line.info(Color::Green);

	let builder = fenrir_rs::Fenrir::builder()
		.endpoint(
			Url::parse(&std::env::var("LOKI_TARGET").expect("no loki URL set"))
				.expect("invalid loki url"),
		)
		.flush_threshold(1_000_000) // prevent overloading loki with too many open connections
		.network(NetworkingBackend::Reqwest)
		.format(SerializationFormat::Json)
		.include_level()
		.tag("service", "stt-service")
		.tag(
			"env",
			if cfg!(debug_assertions) {
				"dev"
			} else {
				"prod"
			},
		);
	let fenrir = builder.build();

	Dispatch::new()
		.chain(
			Dispatch::new()
				// configure this logger instance to make a fancy, colorful output
				.format(move |out, message, record| {
					out.finish(format_args!(
						"{color_line}[{date} {level} {target} {color_line}] {message}\x1B[0m",
						color_line = format_args!(
							"\x1B[{}m",
							colors_line.get_color(&record.level()).to_fg_str()
						),
						date = humantime::format_rfc3339(SystemTime::now()),
						target = record.target(),
						level = colors_level.color(record.level()),
						message = message,
					));
				})
				// just log messages with INFO or higher log level
				.level(tracing::log::LevelFilter::Info)
				// completely ignore ureq logs
				.level_for("ureq", tracing::log::LevelFilter::Off)
				// boost fenrir_rs logs to TRACE
				.level_for("fenrir_rs", tracing::log::LevelFilter::Trace)
				// quieten tracing spans
				.level_for("tracing::span", tracing::log::LevelFilter::Off)
				// print this setup of log messages to the console
				.chain(std::io::stdout()),
		)
		.chain(
			Dispatch::new()
				// output a raw message
				.format(|out, message, _record| out.finish(format_args!("{}", message)))
				// log everything
				.level(tracing::log::LevelFilter::Trace)
				// send this setup of log messages to Loki
				.chain(Box::new(fenrir) as Box<dyn tracing::log::Log>),
		)
		.chain(
			Dispatch::new()
				// configure this logger instance to make a slightly more informative output, but without colors
				.format(move |out, message, record| {
					out.finish(format_args!(
						"[{date} {level} {target}] {message}",
						date = humantime::format_rfc3339(SystemTime::now()),
						target = record.target(),
						level = record.level(),
						message = message,
					))
				})
				// log most things
				.level(tracing::log::LevelFilter::Debug)
				// again, ignore tracing spans
				.level_for("tracing::span", tracing::log::LevelFilter::Off)
				// quieten ureq logs
				.level_for("ureq", tracing::log::LevelFilter::Warn)
				// quieten hyper
				.level_for("hyper", tracing::log::LevelFilter::Warn)
				// quieten h2
				.level_for("h2", tracing::log::LevelFilter::Warn)
				// quieten rustls
				.level_for("rustls", tracing::log::LevelFilter::Warn)
				// send this setup of log messages to a file named `output.log`
				.chain(
					OpenOptions::new()
						.write(true)
						.create(true)
						.truncate(true)
						.open("output.log")
						.expect("failed to open output.log"),
				),
		)
		.apply()
		.expect("failed to init logger");
}

async fn main_inner() {
	stts_speech_to_text::install_log_trampoline();
	init_logging();

	info!("loading models");
	stts_speech_to_text::load_models(
		&std::env::args()
			.nth(1)
			.expect("first argument should be path to model"),
		std::env::args()
			.nth(2)
			.expect("second argument should be how many instances of model to load")
			.parse::<usize>()
			.expect("failed to parse number of instances as usize"),
	);
	info!("loaded models");

	let bind_addr = (
		IpAddr::from([0, 0, 0, 0]),
		match std::env::var("PORT") {
			Ok(port) => port.parse().expect("PORT should be a number"),
			Err(_) => 7269,
		},
	);
	info!("listening on {}:{}", bind_addr.0, bind_addr.1);

	info!("opening socket");
	let socket = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
	info!("opened socket");

	let (shutdown_signal_tx, _shutdown_signal_rx) = tokio::sync::broadcast::channel(1);
	let inner_shutdown_signal_tx = shutdown_signal_tx.clone();
	tokio::spawn(async move {
		tokio::signal::ctrl_c()
			.await
			.expect("failed to listen for ctrl+c");
		inner_shutdown_signal_tx
			.send(())
			.expect("failed to send shutdown signal");
	});

	let stt_streams = Arc::new(DashMap::<Uuid, SttStreamingState>::new());

	info!("polling for connections");
	loop {
		// accept connections and spawn a task for each one
		// or if ctrl+c is received, break the loop
		let conn: tokio::io::Result<(TcpStream, SocketAddr)> = tokio::select! {
			s = socket.accept() => s,
			_ = tokio::signal::ctrl_c() => break,
		};
		debug!("accepting connection");
		match conn {
			Ok((stream, peer_address)) => {
				info!("accepted connection from {}", peer_address);
				let shutdown_rx = shutdown_signal_tx.subscribe();
				let stt_stream_clone = Arc::clone(&stt_streams);
				tokio::spawn(async move {
					let handler = stts_connection_handler::ConnectionHandler::new(
						stream,
						peer_address,
						stt_stream_clone,
						shutdown_rx,
					);
					handler.handle().await;
				});
			}
			Err(e) => println!("accept error: {}", e),
		}
	}
}
