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

Grab a model from <https://github.com/ggerganov/whisper.cpp/blob/master/models/README.md>.
Scripty uses `base` in production, and it runs about 4x faster than real time on a Ryzen 7 7700X.
You can likely get away with only a `.en` model if you don't need multilingual transcription,
but this is untested (we'll still support it if you run into issues!).

Set an env var `LOKI_TARGET` to a URL where a Loki server can be found to push logs to.
Technically, this doesn't need to work at all,
and nothing bad will happen, so you can ignore it if you don't want to set up Loki.
The only requirement is this variable is a valid URL.

If port 7269 is already bound on your system, set the `PORT` env var to a new port to bind to.

Run the binary with two or three arguments:

* first argument should be the path to the STT model you downloaded above.
* second argument should be the maximum number of concurrent transcriptions you wish
  to support
* optional third argument should be a boolean (`true` or `false`) dictating if Scripty is
  allowed to overload this node with jobs: if this is your only node you likely want to set
  this to true.