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
This packet is sent to feed audio into the STT engine.

Do not send this message until you receive a "Initialization Complete" message.
Any packets sent before that will be ignored.

### Type
`0x01`

### Payload
| Field      | Type  | Description                                                |
|------------|-------|------------------------------------------------------------|
| `channels` | `u8`  | The number of channels in the audio data.                  |
| `rate`     | `u32` | The sample rate of the audio data in Hz.                   |
| `data_len` | `u32` | The length of the audio data.                              |
| `data`     | `i16` | The audio data. This must be *exactly* `data_len*2` bytes. |

## Finalize Streaming
This packet is sent to signal the end of audio streaming, and will trigger a "STT Result Packet" to be sent.

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
This packet is sent by the server to signal the end of speech to text processing.

Immediately after the message is sent, the server will close the connection.
### Type
`0x02`
### Payload
| Field    | Type     | Description     |
|----------|----------|-----------------|
| `result` | `String` | The STT result. |

## STT Result Packet (verbose)
This is the same as the normal STT result packet, but includes verbose data.

Just like the normal STT result packet, the connection will be closed after this message is sent.
### Type
`0x03`
### Payload
| Field             | Type     | Description                              |
|-------------------|----------|------------------------------------------|
| `num_transcripts` | `u32`    | The number of transcripts in the result. |
| `main_transcript` | `String` | The main transcript.                     |
| `confidence`      | `f64`    | The confidence of the main transcript.   |

## STT Result Failure
This message is sent when the STT engine fails to run the final result.
### Type
`0x04`
### Payload
| Field   | Type  | Description     |
|---------|-------|-----------------|
| `error` | `i64` | The error code. |
