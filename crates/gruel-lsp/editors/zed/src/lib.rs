use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct GruelExtension;

impl zed::Extension for GruelExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = worktree.which("gruel").ok_or_else(|| {
            "`gruel` not found on PATH. Install the compiler or set \
             `lsp.gruel-lsp.binary.path` in Zed settings."
                .to_string()
        })?;

        Ok(Command {
            command,
            args: vec!["lsp".into()],
            env: Default::default(),
        })
    }
}

zed::register_extension!(GruelExtension);
