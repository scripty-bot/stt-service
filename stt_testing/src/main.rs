use byteorder::{ByteOrder, NetworkEndian, ReadBytesExt, WriteBytesExt};
use std::io;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() {
    let test_file_path = std::env::args()
        .nth(1)
        .expect("first argument should be the path to the WAV file to test");

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

    // open a UDS socket from the client to the server
    // the socket is located at `/tmp/stts.sock`
    let mut socket = UnixStream::connect("/tmp/stts.sock").expect("failed to connect to server");

    // handshake with the server
    println!("sending handshake");
    socket.write_u8(0x00).expect("failed to write to socket");
    socket
        .write_u8(false as u8)
        .expect("failed to write to socket");
    write_string(&mut socket, "en").expect("failed to write to socket");
    socket.flush().expect("failed to flush socket");
    // wait for the server to acknowledge the handshake
    assert_eq!(socket.read_u8().expect("failed to read from socket"), 0x00);
    println!("handshake complete");

    // send the audio data to the server in chunks of 3,840 bytes (20ms, or 240 samples)
    for chunk in data.chunks(240) {
        println!("sending chunk");
        // packet type: 0x01
        socket.write_u8(0x01).expect("failed to write to socket");
        // field 1: number of bytes in the chunk: u32
        let bytes = chunk.len() * std::mem::size_of::<i16>();
        println!("writing {} bytes", bytes);
        socket
            .write_u32::<NetworkEndian>(bytes as u32)
            .expect("failed to write to socket");
        // field 2: chunk data: i16
        let sample_count = chunk.len();
        println!("writing {} samples", sample_count);
        let mut dst = vec![0; sample_count];
        NetworkEndian::write_i16_into(chunk, &mut dst);
        socket.write_all(&dst).expect("failed to write to socket");
        socket.flush().expect("failed to flush socket");
        println!("chunk sent");
    }

    // finalize streaming
    println!("sending finalize streaming");
    socket.write_u8(0x02).expect("failed to write to socket");
    socket.flush().expect("failed to flush socket");
    println!("finalize streaming sent");

    // wait for the server to acknowledge the finalize streaming message
    assert_eq!(socket.read_u8().expect("failed to read from socket"), 0x02);
    // read a string from the server
    let s = read_string(&mut socket).expect("failed to read from socket");
    println!("{}", s);
}

fn read_string(stream: &mut UnixStream) -> io::Result<String> {
    // strings are encoded as a u64 length followed by the string bytes
    let len = stream.read_u64::<NetworkEndian>()?;
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn write_string(stream: &mut UnixStream, string: &str) -> io::Result<()> {
    // strings are encoded as a u64 length followed by the string bytes
    // cache the bytes to prevent a second call to .as_bytes()
    let bytes = string.as_bytes();
    let len = bytes.len() as u64;
    stream.write_u64::<NetworkEndian>(len as u64)?;
    stream.write_all(bytes)?;
    Ok(())
}
