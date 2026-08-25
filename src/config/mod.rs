//! Config directory resolution, `--gen-config`, and loading/validating/selecting from
//! `prompts.yaml`/`api.yaml` for the normal generation flow.

pub mod gen_config;
pub mod listing;
pub mod loader;
pub mod model;
pub mod paths;
pub mod raw;
pub mod validate;
