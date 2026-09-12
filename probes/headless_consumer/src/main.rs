#![forbid(unsafe_code)]

use fcb_headless_consumer::{headless_report, validate_contract};

fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() {
    let contract = headless_report();
    validate_contract(contract).expect("compiled contract must satisfy its own boundary");
    println!(
        "{{\"toolchain\":{},\"edition\":{},\"target\":{},\"deployment_target\":{},\"sdk\":{},\"sandbox_model\":{}}}",
        json_string(contract.toolchain),
        json_string(contract.edition),
        json_string(contract.target),
        json_string(contract.deployment_target),
        json_string(contract.sdk),
        json_string(contract.sandbox_model),
    );
}
