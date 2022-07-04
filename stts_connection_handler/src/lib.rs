//! Connection handler for STTS.

use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

mod tx_types;
mod rx_types;

pub async fn handle_incoming_connection(stream: UnixStream) {
    stream.read_f64()


}
