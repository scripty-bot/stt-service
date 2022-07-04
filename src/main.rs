use std::path::Path;
use tokio::net::unix::SocketAddr;
use tokio::net::UnixStream;

#[tokio::main]
async fn main() {
    stts_speech_to_text::load_models(Path::new(
        &std::env::args()
            .nth(1)
            .expect("first argument should be path to model directory"),
    ));

    let socket =
        tokio::net::UnixListener::bind("/tmp/stts.sock").expect("failed to bind to socket");

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
