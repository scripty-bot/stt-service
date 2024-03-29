# STTS Wire Protocol

STTS implements its own custom wire protocol, that uses Bincode to encode and decode messages.

# Message Format

Each message should be raw binary data, with the following header:

| Field      | Type   | Description                                                |
|------------|--------|------------------------------------------------------------|
| `type`     | `u8`   | The message type                                           |

Immediately following the header is the message payload.

## String Encoding

Strings are encoded as follows:

| Field  | Type  | Description                            |
|--------|-------|----------------------------------------|
| `len`  | `u64` | The length of the string               |
| `data` | `u8`  | The string data (must be valid UTF-8!) |

## Boolean Encoding

Booleans are simply u8s, with 0 for false and 1 for true.

# Client -> Server

## Initialize Streaming

This message is sent by the client to initialize the streaming connection.

### Type

`0x00`

### Payload

| Field      | Type     | Description                               |
|------------|----------|-------------------------------------------|
| `verbose`  | `bool`   | Should the response include verbose data? |
| `language` | `String` | The language to use for the model.        |

## Audio Data Packet

This message is sent to feed audio into the STT engine.

Do not send this message until you receive a "Initialization Complete" message.
Any packets sent before that will be ignored.

### Type

`0x01`

### Payload

| Field      | Type  | Description                                                |
|------------|-------|------------------------------------------------------------|
| `data_len` | `u32` | The length of the audio data.                              |
| `data`     | `i16` | The audio data. This must be *exactly* `data_len*2` bytes. |

## Finalize Streaming

This message is sent to signal the end of audio streaming, and will trigger a "STT Result Packet" to be sent.

### Type

`0x02`

### Payload

None

## Close Connection

This message is sent to close the connection early, and will immediately trigger a close on the server side.

### Type

`0x03`

### Payload

None

## Convert to Status

This message is sent when the client would like this connection to monitor the server status.

### Type

`0x04`

### Payload

None

# Server -> Client

## Initialization Complete

This message is sent by the server to signal that the streaming state has been initialized.

After this message is sent, you may send audio data packets.

### Type

`0x00`

### Payload

None

## Initialization Failed

This message is sent by the server to signal that the streaming state has failed to initialize.

Immediately after the message is sent, the server will close the connection.

### Type

`0x01`

### Payload

| Field      | Type     | Description        |
|------------|----------|--------------------|
| `error`    | `String` | The error message. |

## STT Result Packet (normal)

This message is sent by the server to signal the end of speech to text processing.

Immediately after the message is sent, the server will close the connection.

### Type

`0x02`

### Payload

| Field    | Type     | Description     |
|----------|----------|-----------------|
| `result` | `String` | The STT result. |

## STT Result Packet (verbose)

This is the same as the normal STT result message, but includes verbose data.

Just like the normal STT result message, the connection will be closed after this message is sent.

### Type

`0x03`

### Payload

| Field             | Type     | Description                                                                                 |
|-------------------|----------|---------------------------------------------------------------------------------------------|
| `num_transcripts` | `u32`    | The number of transcripts in the result. If this is 0, this is the only field that is sent. |
| `main_transcript` | `String` | The main transcript.                                                                        |
| `confidence`      | `f64`    | The confidence of the main transcript.                                                      |

## STT Result Failure

This message is sent when the STT engine fails to run the final result.

### Type

`0x04`

### Payload

| Field   | Type  | Description     |
|---------|-------|-----------------|
| `error` | `i64` | The error code. |

## Shutting Down

This message is sent by the server to signal that the server is shutting down.

Immediately after the message is sent, the server will close the connection.

### Type

`0x05`

### Payload

None

## Status Connection Open

This message is sent after the client requests switching the connection into a status connection.

### Type

`0x06`

### Payload

| Field             | Type   | Description                                              |
|-------------------|--------|----------------------------------------------------------|
| `max_utilization` | `f64`  | The maximum utilization of the server.                   |
| `can_overload`    | `bool` | If true, and required, `max_utilization` can be ignored. |

## Status Connection Data

This message is sent every 5 seconds after a "Status Connection Open" message.

### Type

`0x07`

### Payload

| Field             | Type   | Description                            |
|-------------------|--------|----------------------------------------|
| `utilization`     | `f64`  | The current utilization of the server. |

## Fatal IO Error

This message is sent when the server encounters a fatal IO error.

### Type

`0xFD`

### Payload

| Field   | Type     | Description                       |
|---------|----------|-----------------------------------|
| `error` | `String` | The Rust formatted error message. |

## Fatal User Error

This message is sent when the sender sends invalid data.

### Type

`0xFE`

### Payload

None

## Fatal Unknown Error

This message is sent when the server encounters an unknown error.

### Type

`0xFF`

### Payload

None
