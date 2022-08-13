use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use tokio::net::TcpStream;

#[macro_use]
extern crate tracing;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    info!("loading models");
    stts_speech_to_text::load_models(Path::new(
        &std::env::args()
            .nth(1)
            .expect("first argument should be path to model directory"),
    ));
    info!("loaded models");

    info!("opening socket");
    let socket = tokio::net::TcpListener::bind((IpAddr::from([0, 0, 0, 0]), 7269))
        .await
        .unwrap();
    info!("opened socket");

    info!("polling for connections");
    loop {
        // accept connections and spawn a task for each one
        // or if ctrl+c is received, break the loop
        let conn: tokio::io::Result<(TcpStream, SocketAddr)> = tokio::select! {
            s = socket.accept() => s,
            _ = tokio::signal::ctrl_c() => break,
        };
        debug!("accepted connection");
        match conn {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    let mut handler = stts_connection_handler::ConnectionHandler::from(stream);
                    handler.handle().await;
                });
            }
            Err(e) => println!("accept error: {}", e),
        }
    }
}
