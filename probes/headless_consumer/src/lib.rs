#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformContract {
    pub toolchain: &'static str,
    pub edition: &'static str,
    pub target: &'static str,
    pub deployment_target: &'static str,
    pub sdk: &'static str,
    pub sandbox_model: &'static str,
}

pub const SELECTED_CONTRACT: PlatformContract = PlatformContract {
    toolchain: "nightly-2026-09-07",
    edition: "2024",
    target: "aarch64-apple-darwin",
    deployment_target: "14.0",
    sdk: "26.1",
    sandbox_model: "read-only-root-grants",
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractError {
    WrongEdition,
    UndatedToolchain,
    WrongTarget,
    WrongDeploymentTarget,
    WrongSdk,
    MissingSandboxBoundary,
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongEdition => f.write_str("wrong edition"),
            Self::UndatedToolchain => f.write_str("undated toolchain"),
            Self::WrongTarget => f.write_str("wrong target"),
            Self::WrongDeploymentTarget => f.write_str("wrong deployment target"),
            Self::WrongSdk => f.write_str("wrong sdk"),
            Self::MissingSandboxBoundary => f.write_str("missing sandbox boundary"),
        }
    }
}

impl std::error::Error for ContractError {}

pub fn validate_contract(contract: PlatformContract) -> Result<(), ContractError> {
    if contract.edition != "2024" {
        return Err(ContractError::WrongEdition);
    }
    let Some(date) = contract.toolchain.strip_prefix("nightly-") else {
        return Err(ContractError::UndatedToolchain);
    };
    if date.len() != 10
        || date.as_bytes()[4] != b'-'
        || date.as_bytes()[7] != b'-'
        || !date
            .as_bytes()
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return Err(ContractError::UndatedToolchain);
    }
    if contract.target != "aarch64-apple-darwin" {
        return Err(ContractError::WrongTarget);
    }
    if contract.deployment_target != "14.0" {
        return Err(ContractError::WrongDeploymentTarget);
    }
    if contract.sdk != "26.1" {
        return Err(ContractError::WrongSdk);
    }
    if contract.sandbox_model != "read-only-root-grants" {
        return Err(ContractError::MissingSandboxBoundary);
    }
    Ok(())
}

pub fn headless_report() -> PlatformContract {
    SELECTED_CONTRACT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_contract_is_inert_and_valid() {
        assert_eq!(validate_contract(SELECTED_CONTRACT), Ok(()));
        assert_eq!(headless_report(), SELECTED_CONTRACT);
    }
}
