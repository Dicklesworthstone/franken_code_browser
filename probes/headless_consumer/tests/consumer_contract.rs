use fcb_headless_consumer::{validate_contract, ContractError, PlatformContract, SELECTED_CONTRACT};

#[test]
fn headless_consumer_exposes_the_selected_contract() {
    assert_eq!(validate_contract(SELECTED_CONTRACT), Ok(()));
    assert_eq!(SELECTED_CONTRACT.toolchain, "nightly-2026-09-07");
    assert_eq!(SELECTED_CONTRACT.edition, "2024");
    assert_eq!(SELECTED_CONTRACT.sandbox_model, "read-only-root-grants");
}

#[test]
fn consumer_rejects_a_planted_non_2024_contract() {
    let invalid = PlatformContract { edition: "2021", ..SELECTED_CONTRACT };
    assert_eq!(validate_contract(invalid), Err(ContractError::WrongEdition));
}

#[test]
fn consumer_rejects_a_non_apple_target() {
    let invalid = PlatformContract { target: "x86_64-unknown-linux-gnu", ..SELECTED_CONTRACT };
    assert_eq!(validate_contract(invalid), Err(ContractError::WrongTarget));
}

#[test]
fn consumer_rejects_a_platform_below_the_display_link_floor() {
    let invalid = PlatformContract { deployment_target: "13.0", ..SELECTED_CONTRACT };
    assert_eq!(validate_contract(invalid), Err(ContractError::WrongDeploymentTarget));
}

#[test]
fn consumer_rejects_an_unselected_sdk() {
    let invalid = PlatformContract { sdk: "25.0", ..SELECTED_CONTRACT };
    assert_eq!(validate_contract(invalid), Err(ContractError::WrongSdk));
}

#[test]
fn consumer_rejects_an_undated_nightly() {
    let invalid = PlatformContract { toolchain: "nightly", ..SELECTED_CONTRACT };
    assert_eq!(validate_contract(invalid), Err(ContractError::UndatedToolchain));
}

#[test]
fn contract_error_implements_display_and_error() {
    use std::error::Error;
    let err = ContractError::WrongEdition;
    assert_eq!(format!("{err}"), "wrong edition");
    let obj: &dyn Error = &err;
    assert_eq!(obj.to_string(), "wrong edition");

    let sdk_err = ContractError::WrongSdk;
    assert_eq!(format!("{sdk_err}"), "wrong sdk");
}
