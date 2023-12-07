use std::net::{IpAddr, SocketAddr};

use tokio::net::TcpStream;

#[macro_use]
extern crate tracing;

#[tokio::main]
async fn main() {
	tracing_subscriber::fmt::init();

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

	let (shutdown_signal_tx, shutdown_signal_rx) = tokio::sync::broadcast::channel(1);
	let inner_shutdown_signal_tx = shutdown_signal_tx.clone();
	tokio::spawn(async move {
		tokio::signal::ctrl_c()
			.await
			.expect("failed to listen for ctrl+c");
		inner_shutdown_signal_tx
			.send(())
			.expect("failed to send shutdown signal");
	});

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
				tokio::spawn(async move {
					let mut handler = stts_connection_handler::ConnectionHandler::new(
						stream,
						peer_address,
						shutdown_rx,
					);
					handler.handle().await;
				});
			}
			Err(e) => println!("accept error: {}", e),
		}
	}
}
