//! `--gen-config`: create the config directory and write the example
//! `prompts.yaml`/`api.yaml` for whichever of the two don't already exist (prd.md,
//! `--gen-config` under Flags). Never overwrites an existing file.

use crate::error::GenConfigError;
use std::fs;
use std::io;
use std::path::Path;

const PROMPTS_YAML: &str = include_str!("../../assets/prompts.yaml");
const API_YAML: &str = include_str!("../../assets/api.yaml");

/// What happened to one of the two config files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Created,
    AlreadyExisted,
}

/// The outcome for each of the two config files, in the order `ccm` should report them.
#[derive(Debug, Clone, Copy)]
pub struct GenConfigReport {
    pub prompts: Outcome,
    pub api: Outcome,
}

impl GenConfigReport {
    /// The stdout lines `--gen-config` prints, one per file, in a fixed order — so a
    /// partial run (one file created, one already there) reports both outcomes
    /// explicitly rather than only mentioning the one that was created.
    #[must_use]
    pub fn lines(&self) -> [String; 2] {
        [
            describe("prompts.yaml", self.prompts),
            describe("api.yaml", self.api),
        ]
    }
}

fn describe(name: &str, outcome: Outcome) -> String {
    match outcome {
        Outcome::Created => format!("{name}: created"),
        Outcome::AlreadyExisted => format!("{name}: already existed, left untouched"),
    }
}

/// Creates `dir` if it doesn't exist, then writes `prompts.yaml`/`api.yaml` into it for
/// whichever of the two are missing. Any filesystem failure along the way (permission
/// denied, disk full, `dir` already existing as a non-directory, ...) is reported as a
/// [`GenConfigError`].
pub fn gen_config(dir: &Path) -> Result<GenConfigReport, GenConfigError> {
    ensure_dir(dir)?;
    let prompts = write_if_missing(&dir.join("prompts.yaml"), PROMPTS_YAML)?;
    let api = write_if_missing(&dir.join("api.yaml"), API_YAML)?;
    Ok(GenConfigReport { prompts, api })
}

fn ensure_dir(dir: &Path) -> Result<(), GenConfigError> {
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(GenConfigError(format!(
            "{} already exists and is not a directory",
            dir.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => fs::create_dir_all(dir)
            .map_err(|err| GenConfigError(format!("failed to create {}: {err}", dir.display()))),
        Err(err) => Err(GenConfigError(format!(
            "failed to inspect {}: {err}",
            dir.display()
        ))),
    }
}

fn write_if_missing(path: &Path, content: &str) -> Result<Outcome, GenConfigError> {
    match fs::metadata(path) {
        Ok(_) => Ok(Outcome::AlreadyExisted),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::write(path, content).map_err(|err| {
                GenConfigError(format!("failed to write {}: {err}", path.display()))
            })?;
            Ok(Outcome::Created)
        }
        Err(err) => Err(GenConfigError(format!(
            "failed to inspect {}: {err}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_both_files_in_a_fresh_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ccm");
        let report = gen_config(&dir).unwrap();
        assert_eq!(report.prompts, Outcome::Created);
        assert_eq!(report.api, Outcome::Created);
        assert_eq!(
            fs::read_to_string(dir.join("prompts.yaml")).unwrap(),
            PROMPTS_YAML
        );
        assert_eq!(fs::read_to_string(dir.join("api.yaml")).unwrap(), API_YAML);
    }

    #[test]
    fn leaves_both_files_untouched_when_both_already_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ccm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("prompts.yaml"), "custom prompts").unwrap();
        fs::write(dir.join("api.yaml"), "custom api").unwrap();

        let report = gen_config(&dir).unwrap();
        assert_eq!(report.prompts, Outcome::AlreadyExisted);
        assert_eq!(report.api, Outcome::AlreadyExisted);
        assert_eq!(
            fs::read_to_string(dir.join("prompts.yaml")).unwrap(),
            "custom prompts"
        );
        assert_eq!(
            fs::read_to_string(dir.join("api.yaml")).unwrap(),
            "custom api"
        );
    }

    #[test]
    fn reports_a_partial_run_explicitly() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ccm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("api.yaml"), "custom api").unwrap();

        let report = gen_config(&dir).unwrap();
        assert_eq!(report.prompts, Outcome::Created);
        assert_eq!(report.api, Outcome::AlreadyExisted);
        assert_eq!(
            report.lines(),
            [
                "prompts.yaml: created".to_string(),
                "api.yaml: already existed, left untouched".to_string(),
            ]
        );
    }

    #[test]
    fn fails_when_the_config_dir_path_is_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ccm");
        fs::write(&path, "not a directory").unwrap();

        let err = gen_config(&path).unwrap_err();
        assert!(err.0.contains("not a directory"), "{}", err.0);
    }

    #[test]
    fn embedded_assets_are_the_ones_written() {
        assert!(PROMPTS_YAML.contains("template:"));
        assert!(API_YAML.contains("name: OmniRoute"));
        assert!(API_YAML.contains("name: gemini-cli"));
    }
}
