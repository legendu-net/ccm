//! `type: agent_cli` backend (prd.md, `type: agent_cli` fields under Configuration): a
//! subprocess whose argv is `{{prompt}}`/`{{model}}`/`{{system}}`-substituted, and
//! which receives the diff on stdin (stdin is then closed) rather than as an argument —
//! keeping a large diff out of `args` entirely and avoiding `ARG_MAX`.

use super::MessageGenerator;
use crate::config::model::AgentCliEntry;
use crate::error::GenerationError;
use crate::prompt::{self, ResolvedPrompt};
use crate::vcs::exec::{self, ExecError, RunSpec};
use std::path::Path;
use std::time::Duration;

/// Drives one `agent_cli` entry.
pub struct AgentCliGenerator<'a> {
    entry: &'a AgentCliEntry,
    cwd: &'a Path,
}

impl<'a> AgentCliGenerator<'a> {
    #[must_use]
    pub fn new(entry: &'a AgentCliEntry, cwd: &'a Path) -> Self {
        Self { entry, cwd }
    }
}

impl MessageGenerator for AgentCliGenerator<'_> {
    fn generate(
        &self,
        prompt: &ResolvedPrompt<'_>,
        diff: &str,
        _stderr: &mut dyn std::io::Write,
    ) -> Result<String, GenerationError> {
        let args = prompt::substitute_args(&self.entry.args, prompt, &self.entry.model);
        let env: Vec<(String, String)> = self
            .entry
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let spec = RunSpec {
            program: &self.entry.command,
            args: &args,
            cwd: self.cwd,
            stdin: Some(diff.as_bytes()),
            env: &env,
            timeout: Some(Duration::from_secs(self.entry.timeout_secs)),
        };

        let captured = exec::run_capture(&spec)
            .map_err(|err| classify_exec_error(&self.entry.command, err))?;

        if !captured.success {
            return Err(GenerationError::CallFailed(format!(
                "agent CLI '{}' exited with {}: {}",
                self.entry.command,
                exit_description(captured.code),
                exec::owned_utf8_lossy(captured.stderr),
            )));
        }

        Ok(exec::owned_utf8_lossy(captured.stdout))
    }
}

fn exit_description(code: Option<i32>) -> String {
    match code {
        Some(code) => code.to_string(),
        None => "no exit code (killed by signal)".to_string(),
    }
}

fn classify_exec_error(command: &str, err: ExecError) -> GenerationError {
    match err {
        ExecError::TimedOut { after, stderr } => GenerationError::CallFailed(format!(
            "agent CLI '{command}' exceeded its {}s timeout: {}",
            after.as_secs(),
            exec::owned_utf8_lossy(stderr),
        )),
        ExecError::Spawn(source) => {
            GenerationError::CallFailed(format!("agent CLI '{command}' failed to start: {source}"))
        }
        ExecError::Wait(source) => {
            GenerationError::CallFailed(format!("agent CLI '{command}' failed: {source}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(command: &str, args: &[&str], timeout_secs: u64) -> AgentCliEntry {
        AgentCliEntry {
            command: command.to_string(),
            model: "test-model".to_string(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            env: HashMap::new(),
            timeout_secs,
        }
    }

    fn prompt() -> ResolvedPrompt<'static> {
        ResolvedPrompt {
            system: Some("You are helpful."),
            template: "write it",
        }
    }

    fn sh_script(body: &str, timeout_secs: u64) -> AgentCliEntry {
        entry("/bin/sh", &["-c", body], timeout_secs)
    }

    #[test]
    fn substitutes_prompt_and_model_placeholders() {
        // `sh -c script arg0 arg1 arg2` sets $0=arg0, $1=arg1, $2=arg2 inside the
        // script, so $1/$2 below are the substituted {{model}}/{{prompt}} values.
        let e = entry(
            "/bin/sh",
            &[
                "-c",
                "printf 'MODEL=%s PROMPT=%s\\n' \"$1\" \"$2\"",
                "sh",
                "{{model}}",
                "{{prompt}}",
            ],
            30,
        );
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let out = generator
            .generate(&prompt(), "diff content here", &mut Vec::new())
            .unwrap();
        assert_eq!(out, "MODEL=test-model PROMPT=write it\n");
    }

    #[test]
    fn diff_is_readable_on_stdin_and_stdin_is_closed() {
        let e = sh_script("cat", 30);
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let out = generator
            .generate(&prompt(), "the diff\n", &mut Vec::new())
            .unwrap();
        assert_eq!(out, "the diff\n");
    }

    #[test]
    fn blank_output_is_returned_as_is_not_an_error() {
        let e = sh_script("printf '   \\n'", 30);
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let out = generator
            .generate(&prompt(), "diff", &mut Vec::new())
            .unwrap();
        assert_eq!(out, "   \n");
    }

    #[test]
    fn non_zero_exit_is_call_failed_with_stderr_included() {
        let e = sh_script("echo 'Error: invalid model selection' >&2; exit 2", 30);
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let err = generator
            .generate(&prompt(), "diff", &mut Vec::new())
            .unwrap_err();
        match err {
            GenerationError::CallFailed(msg) => {
                assert!(msg.contains("invalid model selection"));
                assert!(msg.contains('2'));
            }
            other => panic!("expected CallFailed, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_is_call_failed_not_a_panic() {
        let e = entry("/definitely/not/a/real/agent-cli", &[], 30);
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        assert!(matches!(
            generator.generate(&prompt(), "diff", &mut Vec::new()),
            Err(GenerationError::CallFailed(_))
        ));
    }

    #[test]
    fn timeout_is_call_failed_with_partial_stderr_included() {
        // AgentCliEntry::timeout_secs is whole seconds (matches the validated config
        // model), so use the smallest real timeout (1s) rather than 0 — a truly
        // instantaneous deadline can race the child before it even writes to stderr.
        let e = sh_script("echo partial >&2; trap '' TERM; sleep 300", 1);
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let start = std::time::Instant::now();
        let err = generator
            .generate(&prompt(), "diff", &mut Vec::new())
            .unwrap_err();
        match err {
            GenerationError::CallFailed(msg) => assert!(msg.contains("partial")),
            other => panic!("expected CallFailed, got {other:?}"),
        }
        assert!(start.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn extra_env_vars_reach_the_child() {
        let mut e = sh_script("echo \"$CCM_TEST_VAR\"", 30);
        e.env
            .insert("CCM_TEST_VAR".to_string(), "hello".to_string());
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let out = generator
            .generate(&prompt(), "diff", &mut Vec::new())
            .unwrap();
        assert_eq!(out, "hello\n");
    }

    #[test]
    fn an_arg_with_no_placeholder_is_untouched() {
        let e = AgentCliEntry {
            args: vec!["-c".to_string(), "echo fixed".to_string()],
            ..sh_script("", 30)
        };
        let cwd = std::env::temp_dir();
        let generator = AgentCliGenerator::new(&e, &cwd);
        let out = generator
            .generate(&prompt(), "diff", &mut Vec::new())
            .unwrap();
        assert_eq!(out, "fixed\n");
    }
}
