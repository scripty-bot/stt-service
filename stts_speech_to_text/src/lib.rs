use once_cell::sync::OnceCell;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex;

pub use whisper_rs::*;

#[macro_use]
extern crate tracing;

pub static MODELS: OnceCell<(
    flume::Sender<WhisperContext>,
    flume::Receiver<WhisperContext>,
)> = OnceCell::new();
pub static WAITING_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn get_load() -> usize {
    WAITING_COUNT.load(Ordering::Relaxed)
}

pub fn load_models(model_path: &str, instances: usize) {
    info!("attempting to load model once");
    let model = WhisperContext::new(model_path).expect("failed to load model");
    info!("loaded model once, now loading {} instances", instances);
    let (sender, receiver) = flume::unbounded();
    sender.send(model).expect("failed to send model to channel");
    for _ in 1..instances {
        sender
            .send(WhisperContext::new(model_path).expect("failed to load model"))
            .expect("failed to send model to channel");
    }
    info!("loaded {} instances of model", instances);
    MODELS
        .set((sender, receiver))
        .expect("failed to set models");
}

fn get_new_model() -> Option<WhisperContext> {
    let models = MODELS.get().expect("models not initialized");
    // try to fetch a model from the pool
    WAITING_COUNT.fetch_add(1, Ordering::SeqCst);
    let maybe_ctx = models.1.recv();
    WAITING_COUNT.fetch_sub(1, Ordering::SeqCst);

    // if we got a model, return it
    // on error, log it and return None
    match maybe_ctx {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            error!("failed to get model from pool (should never happen): {}", e);
            None
        }
    }
}

async fn reap_model_async(model: WhisperContext) {
    trace!("reaping model");
    let models = MODELS.get().expect("models not initialized");
    // try to send the model back to the pool
    // on error, log it and drop the model
    match models.0.send_async(model).await {
        Ok(_) => trace!("reaped model"),
        Err(e) => error!(
            "failed to send model back to pool (should never happen): {}",
            e
        ),
    }
}

fn create_model_params() -> FullParams {
    let mut params = FullParams::new(DecodeStrategy::Greedy { n_past: 0 });
    params.set_n_threads(1);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_print_special_tokens(false);
    params.set_translate(true);

    params
}

/// A wrapper around a Stream that holds the Stream on one thread constantly.
pub struct SttStreamingState {
    stream_data: Mutex<Vec<i16>>,
}

impl SttStreamingState {
    pub fn new() -> Self {
        Self {
            stream_data: Mutex::new(Vec::new()),
        }
    }

    pub async fn feed_audio(&self, mut audio: Vec<i16>) {
        let mut data = self.stream_data.lock().await;
        data.append(&mut audio);
    }

    pub async fn finish_stream(self, verbose: bool) -> Result<String, WhisperError> {
        let Self { stream_data } = self;

        // we own the stream data now, so we can drop the lock
        let audio_data = stream_data.into_inner();

        // use tokio blocking threads to process the audio
        let (model, res) = tokio::task::spawn_blocking(move || {
            let audio_data = audio_data;
            // process to mono 16KHz f32
            // the input is mono 16KHz i16
            let audio_data = convert_integer_to_float_audio_simd(&audio_data);

            // create model params
            let params = create_model_params();

            // get a model from the pool
            let mut model = get_new_model().expect("failed to get model from pool");

            // run the model
            let res = model.full(params, &audio_data);

            // return the model and the result to the async context
            (model, res)
        })
        .await
        .expect("model thread panicked (should never happen)");

        // check if the model failed
        if let Err(e) = res {
            error!("model failed: {:?}", e);
            // reap the model!
            reap_model(model).await;
            return Err(e);
        }

        // get the result
        let num_segments = model.full_n_segments();
        // average english word length is 5.1 characters which we round up to 6
        let mut segments = String::with_capacity(6 * num_segments as usize);
        for i in 0..num_segments {
            match (model.full_get_segment_text(i), verbose) {
                (Ok(s), false) => {
                    segments.push_str(&s);
                    segments.push('\n');
                }
                (Ok(s), true) => {
                    // also add the start and end time
                    let start = model.full_get_segment_t0(i);
                    let end = model.full_get_segment_t1(i);
                    writeln!(segments, "[{} - {}]: {}", start, end, s)
                        .expect("failed to write segment");
                }
                (Err(e), _) => {
                    error!("failed to get segment text: {:?}", e);
                    // reap the model!
                    reap_model(model).await;
                    return Err(e);
                }
            };
        }

        // reap the model!
        reap_model(model).await;

        // return the result
        Ok(segments)
    }
}
