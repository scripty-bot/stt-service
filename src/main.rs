use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[macro_use]
extern crate tracing;

fn main() {
    tracing_subscriber::fmt::init();

    info!("loading models");
    stts_speech_to_text::load_models(Path::new(
        &std::env::args()
            .nth(1)
            .expect("first argument should be path to model directory"),
    ));
    info!("loaded models");

    info!("registering signal handler");
    let exit = Arc::new(AtomicBool::new(false));
    let e2 = exit.clone();
    ctrlc::set_handler(move || {
        e2.store(true, std::sync::atomic::Ordering::SeqCst);
    })
    .expect("failed to register signal handler");
    info!("registered ctrl+c handler");

    info!("opening Unix Domain Socket");
    let socket = UnixListener::bind("/tmp/stts.sock").expect("failed to bind to socket");
    info!("opened Unix Domain Socket");

    info!("polling for connections");
    loop {
        if exit.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // accept connections and spawn a task for each one
        let conn = socket.accept();
        debug!("accepted connection");
        match conn {
            Ok((stream, _)) => {
                std::thread::spawn(move || {
                    let mut handler = stts_connection_handler::ConnectionHandler::from(stream);
                    handler.handle();
                });
            }
            Err(e) => println!("accept error: {}", e),
        }
    }

    info!("caught Ctrl+C, closing Unix Domain Socket");
    drop(socket);
    if let Err(e) = std::fs::remove_file("/tmp/stts.sock") {
        error!("failed to remove socket: {}", e);
    };
    info!("exiting");
}
