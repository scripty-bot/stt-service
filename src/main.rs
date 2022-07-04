use tokio::io::AsyncReadExt;
use tokio::net::unix::SocketAddr;
use tokio::net::UnixStream;

#[tokio::main]
async fn main() {
    let socket = tokio::net::UnixListener::bind("/tmp/stts.sock").expect("failed to bind to socket");

    // create a ctrl+c signal handler
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        tx.send(()).unwrap();
    });

    loop {
        // accept connections and spawn a task for each one
        // or if ctrl+c is received, break the loop
        let conn: tokio::io::Result<(UnixStream, SocketAddr)> = tokio::select! {
            s = socket.accept() => s,
            _ = rx => break,
        };
        match conn {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    stream.read_exact()
                    let mut buf = [0u8; 1024];
                    let n = stream.read(&mut buf).await.unwrap();
                    println!("{}", String::from_utf8_lossy(&buf[..n]));
                });
            }
            Err(e) => println!("accept error: {}", e),
        }

    }
}
