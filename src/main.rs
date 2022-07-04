use std::path::Path;
use tokio::net::unix::SocketAddr;
use tokio::net::UnixStream;

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

    info!("opening Unix Domain Socket");
    let socket =
        tokio::net::UnixListener::bind("/tmp/stts.sock").expect("failed to bind to socket");
    info!("opened Unix Domain Socket");

    info!("polling for connections");
    loop {
        // accept connections and spawn a task for each one
        // or if ctrl+c is received, break the loop
        let conn: tokio::io::Result<(UnixStream, SocketAddr)> = tokio::select! {
            s = socket.accept() => s,
            _ = tokio::signal::ctrl_c() => break,
        };
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
