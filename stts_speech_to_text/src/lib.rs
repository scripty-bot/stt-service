use dashmap::DashMap;
use once_cell::sync::OnceCell;
use std::path::Path;

pub use coqui_stt::*;

#[macro_use]
extern crate tracing;

pub type ModelLocation = (String, Option<String>);
pub static MODELS: OnceCell<DashMap<String, (ModelLocation, Vec<Model>)>> = OnceCell::new();

pub fn load_models(model_dir: &Path) {
    info!("initializing global model map");
    let models = MODELS.get_or_init(DashMap::new);
    info!("finding models in model dir");
    for dir in model_dir.read_dir().expect("IO error") {
        let dir = dir.expect("IO error");
        let dir_path = dir.path();
        if !dir_path.is_dir() {
            continue;
        }
        let file_name = dir.file_name();
        let name = file_name.to_string_lossy();
        if name.len() != 2 {
            continue;
        }
        info!("searching for models in dir {}...", &name);

        let mut model_path = None;
        let mut scorer_path = None;
        for file in dir_path.read_dir().expect("IO error") {
            let file = file.expect("IO error");
            let path = file.path();
            let ext = match path.extension() {
                Some(ext) => ext,
                None => continue,
            };
            if ext == "tflite" {
                model_path = Some(
                    path.to_str()
                        .expect("non-utf-8 chars found in filename")
                        .to_owned(),
                );
            } else if ext == "scorer" {
                scorer_path = Some(
                    path.to_str()
                        .expect("non-utf-8 chars found in filename")
                        .to_owned(),
                );
            }
        }
        if let Some(model_path) = model_path {
            info!("found model: {:?}", model_path);
            info!("loading model");
            let mut model = Model::new(&model_path).expect("failed to load model");
            if let Some(ref scorer_path) = scorer_path {
                info!("using external scorer: {:?}", scorer_path);
                model
                    .enable_external_scorer(scorer_path)
                    .expect("failed to load scorer");
            }
            info!("loaded model, inserting into map");
            models.insert(name.to_string(), ((model_path, scorer_path), vec![model]));
        }
    }
    if models.is_empty() {
        panic!(
            "no models found:\
             they must be in a subdirectory with their language name like `en/model.tflite`"
        )
    } else {
        info!("loaded {} models", models.len());
    }
}

fn get_new_model(lang: &str) -> Option<Model> {
    let models = MODELS.get().expect("models not initialized");
    // try to find a model for the given language
    let mut model_opt = models.get_mut(lang)?;
    if let Some(model) = (model_opt.1).pop() {
        return Some(model);
    }
    // if it wasn't found, make one
    let (model_path, scorer_path) = &model_opt.0;
    let mut model = Model::new(model_path).expect("failed to load model");
    if let Some(ref scorer_path) = scorer_path {
        model
            .enable_external_scorer(scorer_path)
            .expect("failed to load scorer");
    }
    trace!("created new model, now at {} models", model_opt.1.len());
    Some(model)
}

fn reap_model(model: Model, lang: &str) {
    trace!("reaping model");
    let models = MODELS.get().expect("models not initialized");
    if let Some(mut x) = models.get_mut(lang) {
        (x.1).push(model);
        trace!("reaped model successfully");
    }
}

enum StreamTxData {
    FeedAudio(Vec<i16>, flume::Sender<()>),
}

enum StreamTxFinalizeData {
    Finalize(flume::Sender<Result<String>>),
    FinalizeWithMetadata(flume::Sender<Result<Metadata>>),
}

/// A wrapper around a Stream that holds the Stream on one thread constantly.
pub struct SttStreamingState {
    stream_tx: flume::Sender<StreamTxData>,
    final_tx: flume::Sender<StreamTxFinalizeData>,
}

impl SttStreamingState {
    pub async fn new(language: String) -> Option<Self> {
        let (init_tx, init_rx) = flume::bounded(0);
        let (stream_tx, stream_rx) = flume::unbounded();
        let (final_tx, final_rx) = flume::bounded(0);

        std::thread::spawn(move || {
            let model = match get_new_model(&language) {
                Some(model) => model,
                None => {
                    init_tx.send(None).expect("failed to send init message");
                    error!("no model found for language {}", language);
                    return;
                }
            };

            let mut stream = match model.into_streaming() {
                Ok(s) => s,
                Err(e) => {
                    init_tx.send(None).expect("failed to send init message");
                    error!("failed to create stream for model: {}", e);
                    return;
                }
            };
            init_tx.send(Some(())).expect("failed to send init message");

            while let Ok(data) = stream_rx.recv() {
                match data {
                    StreamTxData::FeedAudio(audio, wait_tx) => {
                        stream.feed_audio(&audio);
                        wait_tx.send(()).expect("failed to send wait message");
                    }
                }
            }

            match final_rx.recv() {
                Ok(StreamTxFinalizeData::Finalize(tx)) => {
                    let result = stream.finish_stream();
                    match result {
                        Ok((text, model)) => {
                            reap_model(model, &language);
                            tx.send(Ok(text)).expect("failed to send result");
                        }
                        Err(e) => {
                            tx.send(Err(e)).expect("failed to send result");
                        }
                    }
                }
                Ok(StreamTxFinalizeData::FinalizeWithMetadata(tx)) => {
                    let result = stream.finish_stream_with_metadata(10);
                    match result {
                        Ok((metadata, model)) => {
                            reap_model(model, &language);
                            tx.send(Ok(metadata)).expect("failed to send result");
                        }
                        Err(e) => {
                            tx.send(Err(e)).expect("failed to send result");
                        }
                    }
                }
                Err(e) => {
                    error!("failed to receive finalize message: {}", e);
                }
            };
        });

        init_rx.recv_async().await.ok().flatten()?;

        Some(SttStreamingState {
            stream_tx,
            final_tx,
        })
    }

    pub async fn feed_audio(&self, audio: Vec<i16>) {
        let (wait_tx, wait_rx) = flume::bounded(0);

        self.stream_tx
            .send(StreamTxData::FeedAudio(audio, wait_tx))
            .expect("failed to send message");

        let _ = wait_rx.recv_async().await;
    }

    pub async fn finish_stream(self) -> Result<String> {
        let Self {
            stream_tx,
            final_tx,
        } = self;
        // drop the stream to immediately throw an error in the spawned thread
        drop(stream_tx);

        let (tx, rx) = flume::bounded(0);
        final_tx
            .send(StreamTxFinalizeData::Finalize(tx))
            .expect("failed to send message");
        rx.recv_async().await.expect("failed to receive result")
    }

    pub async fn finish_stream_with_metadata(self) -> Result<Metadata> {
        let Self {
            stream_tx,
            final_tx,
        } = self;
        // drop the stream to immediately throw an error in the spawned thread
        drop(stream_tx);

        let (tx, rx) = flume::bounded(0);
        final_tx
            .send(StreamTxFinalizeData::FinalizeWithMetadata(tx))
            .expect("failed to send message");
        rx.recv_async().await.expect("failed to receive result")
    }
}
