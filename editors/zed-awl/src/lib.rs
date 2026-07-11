use zed_extension_api::{
    self as zed,
    settings::{CommandSettings, LspSettings},
    LanguageServerId, Result,
};

struct AwlExtension;

fn server_command(
    binary: Option<CommandSettings>,
    path_aion: Option<String>,
) -> Result<zed::Command> {
    let command = binary
        .as_ref()
        .and_then(|binary| binary.path.clone())
        .or(path_aion)
        .ok_or_else(|| {
            "AWL language support requires the `aion` executable in PATH or an explicit `lsp.awl.binary.path` setting"
                .to_string()
        })?;

    let args = binary
        .as_ref()
        .and_then(|binary| binary.arguments.clone())
        .unwrap_or_else(|| vec!["awl".to_string(), "lsp".to_string()]);
    let env = binary
        .and_then(|binary| binary.env)
        .unwrap_or_default()
        .into_iter()
        .collect();

    Ok(zed::Command { command, args, env })
}

impl zed::Extension for AwlExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        server_command(settings.binary, worktree.which("aion"))
    }
}

zed::register_extension!(AwlExtension);

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn uses_path_binary_with_default_awl_lsp_arguments() {
        let command = server_command(None, Some("/usr/local/bin/aion".to_string()))
            .expect("PATH binary should produce a command");

        assert_eq!(command.command, "/usr/local/bin/aion");
        assert_eq!(command.args, ["awl", "lsp"]);
        assert!(command.env.is_empty());
    }

    #[test]
    fn settings_override_path_arguments_and_environment() {
        let command = server_command(
            Some(CommandSettings {
                path: Some("/opt/aion/bin/aion".to_string()),
                arguments: Some(vec![
                    "awl".to_string(),
                    "lsp".to_string(),
                    "--trace".to_string(),
                ]),
                env: Some(HashMap::from([(
                    "RUST_LOG".to_string(),
                    "debug".to_string(),
                )])),
            }),
            Some("/usr/local/bin/aion".to_string()),
        )
        .expect("settings binary should produce a command");

        assert_eq!(command.command, "/opt/aion/bin/aion");
        assert_eq!(command.args, ["awl", "lsp", "--trace"]);
        assert_eq!(command.env, [("RUST_LOG".to_string(), "debug".to_string())]);
    }

    #[test]
    fn reports_how_to_configure_a_missing_binary() {
        let error = server_command(None, None).expect_err("missing aion should fail");

        assert!(error.contains("aion"));
        assert!(error.contains("lsp.awl.binary.path"));
    }
}
