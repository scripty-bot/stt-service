# stt-service

The Scripty STT service is a simple server that handles remote audio transcoding.

## Building

This program is written in [Rust](https://rust-lang.org/). You will need to [install rust](https://rustup.rs) to build it.
You will also need the libstdd.tflite.(os).zip file from [coqui-ai/STT](https://github.com/coqui-ai/STT/releases/). You should put them in /usr/local/lib.

Then, you can build with `cargo build --release`.

## Running

After you've built stt-service, you need some models. These can be had easily at [coqui.ai/models](https://coqui.ai/models). You need the model.tflite and the .scorer file.
For each language, you need to make a subfolder with the two letter language code (e.g. en, fr, de). Then put the models in them, and run `scripty_stt_service /path/to/models/`.

Voila! You've got a working stt-service instance!
