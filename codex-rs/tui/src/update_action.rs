/// Update action the CLI should perform after the TUI exits.
///
/// XLI distributes through a single curl installer.
/// No npm, no bun, no homebrew, no standalone detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Re-run the XLI installer to pull the latest binary.
    CurlInstall,
}

const XLI_INSTALL_CMD: &str = "";

impl UpdateAction {
    /// Returns the command-line arguments for invoking the update.
    pub fn command_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            UpdateAction::CurlInstall => ("bash", &["-c", XLI_INSTALL_CMD]),
        }
    }

    /// Returns a human-readable command string for display in the update prompt.
    pub fn command_str(self) -> String {
        match self {
            UpdateAction::CurlInstall => XLI_INSTALL_CMD.to_string(),
        }
    }
}

#[cfg(not(debug_assertions))]
pub fn get_update_action() -> Option<UpdateAction> {
    Some(UpdateAction::CurlInstall)
}
