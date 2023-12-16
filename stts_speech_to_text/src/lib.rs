use std::{
	fmt::Write,
	sync::{
		atomic::{AtomicUsize, Ordering},
		Arc,
		OnceLock as OnceCell,
	},
	time::Instant,
};

use parking_lot::Mutex;
use tokio::sync::Semaphore;
pub use whisper_rs::*;

#[macro_use]
extern crate tracing;

pub static MODEL: OnceCell<WhisperContext> = OnceCell::new();
pub static MAX_CONCURRENCY: OnceCell<Arc<Semaphore>> = OnceCell::new();
pub static WAITING_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn install_log_trampoline() {
	#[no_mangle]
	pub unsafe extern "C" fn whisper_log_trampoline(
		level: whisper_rs_sys::ggml_log_level,
		text: *const std::os::raw::c_char,
		user_data: *mut std::os::raw::c_void,
	) {
		let message = match unsafe { std::ffi::CStr::from_ptr(text) }.to_str() {
			Ok(s) => s,
			Err(e) => {
				error!("whisper: log string has invalid UTF-8: {}", e);
				return;
			}
		};

		match level {
			whisper_rs_sys::ggml_log_level_GGML_LOG_LEVEL_INFO => {
				info!("whisper: {}", message);
			}
			whisper_rs_sys::ggml_log_level_GGML_LOG_LEVEL_WARN => {
				warn!("whisper: {}", message);
			}
			whisper_rs_sys::ggml_log_level_GGML_LOG_LEVEL_ERROR => {
				error!("whisper: {}", message);
			}
			_ => {
				error!("whisper: unknown log level: {}: {}", level, message);
			}
		}
	}

	unsafe { set_log_callback(Some(whisper_log_trampoline), std::ptr::null_mut()) };
}

pub fn get_load() -> usize {
	WAITING_COUNT.load(Ordering::Relaxed)
}

pub fn load_models(model_path: &str, instances: usize) {
	info!("attempting to load model");
	let model = WhisperContext::new_with_params(model_path, WhisperContextParameters::new())
		.expect("failed to load model");
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

fn create_model_params(language: &str, translate: bool) -> FullParams {
	let mut params = FullParams::new(SamplingStrategy::BeamSearch {
		beam_size: 5,
		patience:  -1.0,
	});
	params.set_n_threads(1);
	params.set_print_progress(false);
	params.set_print_realtime(false);
	params.set_print_timestamps(false);
	params.set_no_context(true);
	params.set_suppress_non_speech_tokens(true);
	params.set_language(Some(language));
	params.set_translate(translate);

	params
}

/// A wrapper around a Stream that holds the Stream on one thread constantly.
pub struct SttStreamingState {
	stream_data: Mutex<Vec<i16>>,
	last_access: Mutex<Instant>,
}

impl Default for SttStreamingState {
	fn default() -> Self {
		Self::new()
	}
}

impl SttStreamingState {
	pub fn new() -> Self {
		Self {
			stream_data: Mutex::new(Vec::new()),
			last_access: Mutex::new(Instant::now()),
		}
	}

	pub fn feed_audio(&self, mut audio: Vec<i16>) {
		self.stream_data.lock().append(&mut audio);
		self.update_last_access();
	}

	pub async fn finish_stream(
		self,
		language: String,
		verbose: bool,
		translate: bool,
	) -> Result<String, WhisperError> {
		let Self {
			stream_data,
			last_access: _,
		} = self;

		// we own the stream data now, so we can drop the lock
		let audio_data = stream_data.into_inner();

		WAITING_COUNT.fetch_add(1, Ordering::Relaxed);
		let permit = MAX_CONCURRENCY
			.get()
			.expect("max concurrency not set")
			.clone()
			.acquire_owned()
			.await
			.expect("semaphore should not be closed");
		WAITING_COUNT.fetch_sub(1, Ordering::Relaxed);

		// use tokio blocking threads to process the audio
		let (state, res) = tokio::task::spawn_blocking(move || {
			// process to mono 16KHz f32
			// the input is mono 16KHz i16
			let audio_data = convert_integer_to_float_audio(&audio_data);

			// create model params
			let params = create_model_params(&language, translate);

			// get a model from the pool
			let mut state = get_new_model().expect("failed to get model from pool");

			// run the model
			let res = state.full(params, &audio_data);

			// return the model and the result to the async context
			(state, res)
		})
		.await
		.map_err(|e| {
			error!("failed to spawn blocking task: {:?}", e);
			// TODO: better error
			//  we hijacked this error,
			//  as it's impossible to get in this code path
			WhisperError::InvalidMelBands
		})?;
		drop(permit);

		// check if the model failed
		if let Err(e) = res {
			error!("model failed: {:?}", e);
			return Err(e);
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
					return Err(e);
				}
			};
		}

		// return the result
		Ok(segments)
	}

	fn update_last_access(&self) {
		let mut last_access = self.last_access.lock();
		*last_access = Instant::now();
	}

	pub fn get_last_access(&self) -> Instant {
		*self.last_access.lock()
	}
}
