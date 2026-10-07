//! dbxctl's own exit codes, in one place (#23).
//!
//! Passthrough commands return the Databricks CLI's exit code unchanged; these
//! codes apply only to outcomes that dbxctl itself decides.

use std::process::ExitCode;

/// An exit outcome decided by dbxctl.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Exit {
    /// The command completed. For `probe run`, every selected check resolved.
    Success,
    /// An invocation or usage failure, such as an invalid option or an
    /// unwritable run directory.
    Usage,
    /// A probe run completed, but one or more checks are unknown or blocked.
    Unresolved,
    /// Probe cleanup could not remove every created resource.
    // Constructed by `probe cleanup` once it is implemented.
    #[allow(dead_code)]
    Cleanup,
}

impl Exit {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Usage => 1,
            Self::Unresolved => 10,
            Self::Cleanup => 11,
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

#[cfg(test)]
mod tests {
    use super::Exit;

    #[test]
    fn codes_match_the_phase_0_contract() {
        assert_eq!(
            [Exit::Success, Exit::Usage, Exit::Unresolved, Exit::Cleanup].map(Exit::code),
            [0, 1, 10, 11]
        );
    }
}
