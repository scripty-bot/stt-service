use once_cell::sync::OnceCell;
use std::fmt;
use std::fmt::{Formatter, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};

pub use whisper_rs::*;

#[macro_use]
extern crate tracing;

pub static MODEL: OnceCell<WhisperContext> = OnceCell::new();
pub static MAX_CONCURRENCY: OnceCell<Arc<Semaphore>> = OnceCell::new();
pub static WAITING_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn get_load() -> usize {
    WAITING_COUNT.load(Ordering::Relaxed)
}

pub fn load_models(model_path: &str, instances: usize) {
    info!("attempting to load model");
    let model = WhisperContext::new(model_path).expect("failed to load model");
    MODEL.set(model).expect("failed to set models");
    info!("loaded model");

    info!("max concurrency: {}", instances);
    MAX_CONCURRENCY
        .set(Arc::new(Semaphore::new(instances)))
        .expect("failed to set max concurrency");
}

fn get_new_model() -> Option<WhisperState<'static>> {
    // if we got a model, return it
    // on error, log it and return None
    match MODEL.get().map(|ctx| ctx.create_state()) {
        Some(Ok(state)) => Some(state),
        Some(Err(e)) => {
            error!("failed to create model: {:?}", e);
            None
        }
        None => {
            error!("models not set up yet: check that load_models was called");
            None
        }
    }
}

fn create_model_params(language: &str) -> FullParams {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(1);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_context(true);
    params.set_suppress_non_speech_tokens(true);
    params.set_language(Some(language));

    params
}

#[derive(Debug, Copy, Clone)]
pub enum SttError {
    Whisper(WhisperError),
    Timeout,
}

impl From<WhisperError> for SttError {
    fn from(e: WhisperError) -> Self {
        Self::Whisper(e)
    }
}

impl From<tokio::time::error::Elapsed> for SttError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Self::Timeout
    }
}

impl std::error::Error for SttError {}

impl fmt::Display for SttError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Whisper(e) => write!(f, "Whisper error: {}", e),
            Self::Timeout => write!(f, "Timeout"),
        }
    }
}

impl Into<i64> for SttError {
    fn into(self) -> i64 {
        match self {
            Self::Whisper(_) => -1,
            Self::Timeout => i64::MIN,
        }
    }
}

/// A wrapper around a Stream that holds the Stream on one thread constantly.
pub struct SttStreamingState {
    stream_data: Mutex<Vec<i16>>,
    language: String,
}

impl SttStreamingState {
    pub fn new(language: String) -> Self {
        Self {
            stream_data: Mutex::new(Vec::new()),
            language,
        }
    }

    pub async fn feed_audio(&self, mut audio: Vec<i16>) {
        let mut data = self.stream_data.lock().await;
        data.append(&mut audio);
    }

    pub async fn finish_stream(self, verbose: bool) -> Result<String, SttError> {
        let Self {
            stream_data,
            language,
        } = self;

        // we own the stream data now, so we can drop the lock
        let audio_data = stream_data.into_inner();

        debug!("waiting for semaphore");
        let permit = MAX_CONCURRENCY
            .get()
            .expect("max concurrency not set")
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed");
        debug!("got semaphore");

        // use tokio blocking threads to process the audio
        let audio_process_task = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::task::spawn_blocking(move || {
                // process to mono 16KHz f32
                // the input is mono 16KHz i16
                let audio_data = convert_integer_to_float_audio_simd(&audio_data);

                // create model params
                let params = create_model_params(&language);

                // get a model from the pool
                let mut state = get_new_model().expect("failed to get model from pool");

                // run the model
                let res = state.full(params, &audio_data);

                // for all intents and purposes, we are done with the model
                drop(permit);

                // return the model and the result to the async context
                (state, res)
            }),
        );
        let (state, res) = audio_process_task
            .await?
            .expect("model thread panicked (should never happen)");

        // check if the model failed
        if let Err(e) = res {
            error!("model failed: {:?}", e);
            return Err(e.into());
        }

        // get the result
        let num_segments = state.full_n_segments()?;
        // average english word length is 5.1 characters which we round up to 6
        let mut segments = String::with_capacity(6 * num_segments as usize);
        for i in 0..num_segments {
            match (state.full_get_segment_text(i), verbose) {
                (Ok(s), false) => {
                    segments.push_str(&s);
                    segments.push('\n');
                }
                (Ok(s), true) => {
                    // also add the start and end time
                    let start = state.full_get_segment_t0(i)?;
                    let end = state.full_get_segment_t1(i)?;
                    writeln!(segments, "[{} - {}]: {}", start, end, s)
                        .expect("failed to write segment");
                }
                (Err(e), _) => {
                    error!("failed to get segment text: {:?}", e);
                    return Err(e.into());
                }
            };
        }

        // return the result
        Ok(segments)
    }
}
