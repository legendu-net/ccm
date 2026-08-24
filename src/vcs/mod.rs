//! git/jj subprocess interaction: argv construction, `jj diff --summary` parsing,
//! path normalization, and `--include`/`--exclude` scope resolution.

pub mod argv;
pub mod exec;
pub mod pathnorm;
pub mod scope;
pub mod summary;
