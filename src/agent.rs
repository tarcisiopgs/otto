use serde::Deserialize;

/// A coding agent otto knows how to start without a terminal attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// The argv of a non-interactive run. `extra` comes from the job (permission
    /// flags, model…) and goes before the prompt, which is always last.
    pub fn command(self, prompt: &str, extra: &[String]) -> Vec<String> {
        let head: &[&str] = match self {
            Agent::Claude => &["claude", "-p"],
            Agent::Codex => &["codex", "exec"],
        };
        head.iter()
            .map(|part| (*part).to_owned())
            .chain(extra.iter().cloned())
            .chain(std::iter::once(prompt.to_owned()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_the_last_argument() {
        let extra = vec!["--model".to_owned(), "sonnet".to_owned()];
        assert_eq!(
            Agent::Claude.command("do it", &extra),
            ["claude", "-p", "--model", "sonnet", "do it"]
        );
        assert_eq!(
            Agent::Codex.command("do it", &[]),
            ["codex", "exec", "do it"]
        );
    }
}
