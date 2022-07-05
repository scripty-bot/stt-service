use dashmap::DashMap;
use once_cell::sync::OnceCell;
use std::path::Path;

pub use coqui_stt::*;

#[macro_use]
extern crate tracing;

pub type ModelLocation = (String, Option<String>);
pub static MODELS: OnceCell<DashMap<String, ModelLocation>> = OnceCell::new();

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
            models.insert(name.to_string(), (model_path, scorer_path));
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

pub fn get_new_model(lang: &str) -> Option<Model> {
    let models = MODELS.get().expect("models not initialized");
    // make a model for the language if possible
    let model_location = models.get(lang)?;
    let (model_path, scorer_path) = &model_location.value();
    let mut model = Model::new(model_path).expect("failed to load model");
    if let Some(ref scorer_path) = scorer_path {
        model
            .enable_external_scorer(scorer_path)
            .expect("failed to load scorer");
    }
    trace!("created new model");
    Some(model)
}
