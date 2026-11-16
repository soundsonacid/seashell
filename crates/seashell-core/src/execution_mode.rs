use crate::error::SeashellError;

/// How seashell executes SBF programs.
///
/// Today every mode is one of solana-sbpf's and carries its name. Modes of other VMs get added
/// here, which is why the conversion to the solana-sbpf mode is fallible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    /// The solana-sbpf interpreter.
    Interpreted,
    /// The solana-sbpf JIT.
    Jit,
    /// The solana-sbpf JIT if the program is compiled, its interpreter otherwise.
    #[default]
    PreferJit,
    /// The parrot interpreter of solana-sbpf.
    ParrotInterpreted,
    /// The parrot JIT of solana-sbpf.
    ParrotJit,
}

impl TryFrom<ExecutionMode> for solana_sbpf::vm::ExecutionMode {
    type Error = SeashellError;

    /// Fails with [`SeashellError::NotAnSbpfExecutionMode`] for the modes solana-sbpf does not
    /// run, of which there are none yet.
    fn try_from(mode: ExecutionMode) -> Result<Self, Self::Error> {
        match mode {
            ExecutionMode::Interpreted => Ok(Self::Interpreted),
            ExecutionMode::Jit => Ok(Self::Jit),
            ExecutionMode::PreferJit => Ok(Self::PreferJit),
            ExecutionMode::ParrotInterpreted => Ok(Self::ParrotInterpreted),
            ExecutionMode::ParrotJit => Ok(Self::ParrotJit),
        }
    }
}
