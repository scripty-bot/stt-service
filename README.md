# ARCHIVED
visit https://codeberg.org/scripty-bot/scripty for any new code

# stt-service

The Scripty STT service is a simple server that handles remote audio transcription.

## Building

Should just be a `cargo build --release` if you're fine with running exclusively on CPU.
(Scripty runs on CPU only in production, modern Ryzen CPUs are incredibly powerful with AVX512)

The STT service also supports some GPU acceleration.
Poke through Cargo.toml here at the root and look under the features section.
To enable them, either add them to the default array,
or run the build command with `--features opencl` appended,
substituting `opencl` for whatever you want.

## Running

Scripty's STT service is designed to integrate with systemd.
Docker is possible but we offer limited support for it, and you have to figure that out yourself.
These official steps will use systemd.

Copy `target/release/scripty_stt_service` to `/usr/local/bin`.

Grab a model from <https://github.com/ggml-org/whisper.cpp/blob/master/models/README.md>.
Scripty uses `base` in production, and it runs about 4x faster than real time on a Ryzen 7 7700X.
You can likely get away with only a `.en` model if you don't need multilingual transcription,
but this is untested (we'll still support it if you run into issues!).
You should place this model in `/usr/share/whisper-models`.

Modify `util/scripty-stt-service.service`, and change the following:

* `Environment="LOKI_TARGET=http://127.0.0.1:3100/"`:
  leave this as-is if you don't have Loki set up, or if it's running on localhost.
  Otherwise, modify it to point to your Loki instance.
* `ExecStart=/usr/local/bin/scripty_stt_service /usr/share/whisper-models/ggml-base-q5_1.bin 14 true`:
    * modify the path to the whisper model if you have them stored somewhere else,
      or change the name if you're not using the quantized base model.
    * change the 14 to the number of threads you're willing to let the STT service use
    * change true to false if you do not want Scripty to be able to overload this node with jobs
      (do not change this if you don't know what you're doing and the consequences)
* `ReadOnlyPaths=/usr/share/whisper-models/`:
  if your models are stored somewhere besides `/usr/share/whisper-models`, modify this to that path.

Copy `util/scripty-stt-service.service` and `util/scripty-stt-service.socket` to `/etc/systemd/system`.
If port 7269 is already bound on your system, edit `scripty-stt-service.socket` to change the listen port.
To actually start it listening, run `sudo systemctl enable --now scripty-stt-service.socket`.

You can use the testing binary in `stt_testing/` to test your setup.
Run the following from the same directory as this README:

```bash
cd stt_testing/
cargo build --release
./target/release/stt_testing ../2830-3980-0043.wav
```

If this succeeds without error, and you get a result of "Experience proves this.", your setup is working!
Continue onwards to setting up Scripty.
