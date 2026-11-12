// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT

use std::collections::HashMap;

use serde_json::{json, Value};

use reqwest::StatusCode;

use super::lockdown_tests::{client, get, patch, Request};
use crate::{BiosProfileType, Boot, Redfish, RedfishError};

const BIOS: &str = "Systems/1/Bios";
const BIOS_PENDING: &str = "Systems/1/Bios/Pending";
const BOOT_OPTIONS: &str = "Systems/1/BootOptions";
const BOOT_OPTION_NETWORK: &str = "Systems/1/BootOptions/Boot0003";
const BOOT_OPTION_HARD_DISK: &str = "Systems/1/BootOptions/Boot0002";
const SYSTEM_PENDING: &str = "Systems/1/Pending";
const BOOT_SETTINGS: &str = "Systems/1/Oem/Lenovo/BootSettings";
const GENERAL_BOOT_ORDER: &str = "Systems/1/Oem/Lenovo/BootSettings/BootOrder.BootOrder";

fn mockup(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

fn boot_settings() -> Value {
    mockup(include_str!(
        "../../tests/mockups/lenovo/redfish/v1/Systems/1/Oem/Lenovo/BootSettings/index.json"
    ))
}

fn general_boot_order(next: &[&str]) -> Value {
    json!({
        "BootOrderCurrent": next,
        "BootOrderNext": next,
        "BootOrderSupported": ["Network", "Hard Disk", "CD/DVD Rom", "USB Storage"],
    })
}

fn bios_ready() -> Value {
    json!({
        "Attributes": {
            "Processors_IntelVirtualizationTechnology": "Enabled",
            "BootModes_SystemBootMode": "UEFIMode",
            "NetworkStackSettings_IPv4HTTPSupport": "Enabled",
            "NetworkStackSettings_IPv4PXESupport": "Disabled",
            "NetworkStackSettings_IPv6PXESupport": "Disabled",
            "BootModes_InfiniteBootRetry": "Enabled",
            "BootModes_PreventOSChangesToBootOrder": "Enabled",
        }
    })
}

#[tokio::test]
async fn bios_diff_reports_network_dropped_from_general_boot_order() {
    let (bmc, server) = client(vec![
        get(BIOS, bios_ready()),
        get(BIOS, bios_ready()),
        get(BIOS, bios_ready()),
        get(
            "Systems/1/",
            mockup(include_str!(
                "../../tests/mockups/lenovo/redfish/v1/Systems/1/index.json"
            )),
        ),
        get(
            "Systems/1/BootOptions/Boot0003",
            mockup(include_str!(
                "../../tests/mockups/lenovo/redfish/v1/Systems/1/BootOptions/Boot0003/index.json"
            )),
        ),
        get(BOOT_SETTINGS, boot_settings()),
        get(GENERAL_BOOT_ORDER, general_boot_order(&["Hard Disk"])),
    ])
    .await;

    let diffs = bmc.diff_bios_bmc_attr().await.unwrap();
    server.finish().await;

    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert_eq!(diffs[0].key, "general_boot_order_network");
    assert_eq!(diffs[0].expected, "Network");
    assert_eq!(diffs[0].actual, "_missing_");
}

fn patch_any(path: &'static str) -> Request {
    Request {
        method: "PATCH",
        path,
        body: None,
        status: 204,
        response: Value::Null,
    }
}

fn not_found(path: &'static str) -> Request {
    Request {
        method: "GET",
        path,
        body: None,
        status: 404,
        response: json!({}),
    }
}

fn boot_options(members: &[&str]) -> Value {
    let mut collection = mockup(include_str!(
        "../../tests/mockups/lenovo/redfish/v1/Systems/1/BootOptions/index.json"
    ));
    collection["Members"] = members
        .iter()
        .map(|path| json!({"@odata.id": format!("/redfish/v1/{path}")}))
        .collect();
    collection
}

fn network_boot_option() -> Value {
    mockup(include_str!(
        "../../tests/mockups/lenovo/redfish/v1/Systems/1/BootOptions/Boot0003/index.json"
    ))
}

fn hard_disk_boot_option() -> Value {
    mockup(include_str!(
        "../../tests/mockups/lenovo/redfish/v1/Systems/1/BootOptions/Boot0002/index.json"
    ))
}

fn serial_console_and_tpm() -> Vec<Request> {
    vec![
        get(BIOS, bios_ready()),
        patch_any(BIOS_PENDING),
        patch(
            BIOS_PENDING,
            json!({"Attributes": {"TrustedComputingGroup_DeviceOperation": "Clear"}}),
        ),
    ]
}

fn network_boot_first() -> Vec<Request> {
    vec![
        get(
            BOOT_OPTIONS,
            boot_options(&[BOOT_OPTION_NETWORK, BOOT_OPTION_HARD_DISK]),
        ),
        get(BOOT_OPTION_NETWORK, network_boot_option()),
        get(BOOT_OPTION_HARD_DISK, hard_disk_boot_option()),
        patch(
            SYSTEM_PENDING,
            json!({"Boot": {"BootOrder": ["Boot0003", "Boot0002"]}}),
        ),
    ]
}

fn network_boot_option_missing() -> Vec<Request> {
    vec![
        get(BOOT_OPTIONS, boot_options(&[BOOT_OPTION_HARD_DISK])),
        get(BOOT_OPTION_HARD_DISK, hard_disk_boot_option()),
    ]
}

fn virtualization_and_uefi_boot_only() -> Vec<Request> {
    vec![
        get(BIOS, bios_ready()),
        patch(
            BIOS_PENDING,
            json!({"Attributes": {"Processors_IntelVirtualizationTechnology": "Enabled"}}),
        ),
        get(BIOS, bios_ready()),
        patch_any(BIOS_PENDING),
    ]
}

async fn run_machine_setup(script: Vec<Vec<Request>>) {
    let (bmc, server) = client(script.concat()).await;
    let result = bmc
        .machine_setup(
            None,
            &HashMap::new(),
            BiosProfileType::Performance,
            &HashMap::new(),
        )
        .await;
    server.finish().await;
    assert_eq!(result.unwrap(), None);
}

#[tokio::test]
async fn machine_setup_restores_network_before_setting_boot_first() {
    run_machine_setup(vec![
        serial_console_and_tpm(),
        vec![
            get(BOOT_SETTINGS, boot_settings()),
            get(GENERAL_BOOT_ORDER, general_boot_order(&["Hard Disk"])),
            patch(
                GENERAL_BOOT_ORDER,
                json!({"BootOrderNext": ["Network", "Hard Disk"]}),
            ),
        ],
        network_boot_first(),
        virtualization_and_uefi_boot_only(),
    ])
    .await;
}

#[tokio::test]
async fn machine_setup_falls_back_to_general_boot_order_when_network_option_is_missing() {
    run_machine_setup(vec![
        serial_console_and_tpm(),
        vec![
            get(BOOT_SETTINGS, boot_settings()),
            get(GENERAL_BOOT_ORDER, general_boot_order(&["Hard Disk"])),
            patch(
                GENERAL_BOOT_ORDER,
                json!({"BootOrderNext": ["Network", "Hard Disk"]}),
            ),
        ],
        network_boot_option_missing(),
        vec![
            get(BOOT_SETTINGS, boot_settings()),
            get(
                GENERAL_BOOT_ORDER,
                general_boot_order(&["Network", "Hard Disk"]),
            ),
        ],
        virtualization_and_uefi_boot_only(),
    ])
    .await;
}

#[tokio::test]
async fn machine_setup_keeps_general_boot_order_when_network_is_present() {
    run_machine_setup(vec![
        serial_console_and_tpm(),
        vec![
            get(BOOT_SETTINGS, boot_settings()),
            get(
                GENERAL_BOOT_ORDER,
                general_boot_order(&["Network", "Hard Disk"]),
            ),
        ],
        network_boot_first(),
        virtualization_and_uefi_boot_only(),
    ])
    .await;
}

#[tokio::test]
async fn machine_setup_skips_general_boot_order_when_absent() {
    run_machine_setup(vec![
        serial_console_and_tpm(),
        vec![
            get(BOOT_SETTINGS, boot_settings()),
            not_found(GENERAL_BOOT_ORDER),
        ],
        network_boot_option_missing(),
        vec![
            get(BOOT_SETTINGS, boot_settings()),
            not_found(GENERAL_BOOT_ORDER),
        ],
        virtualization_and_uefi_boot_only(),
    ])
    .await;
}

#[tokio::test]
async fn network_restore_returns_not_found_when_general_boot_order_is_absent() {
    let (bmc, server) = client(vec![
        get(BOOT_SETTINGS, boot_settings()),
        not_found(GENERAL_BOOT_ORDER),
    ])
    .await;

    let result = bmc.ensure_in_general_boot_order(Boot::Pxe).await;
    server.finish().await;
    assert!(
        matches!(
            result,
            Err(RedfishError::HTTPErrorCode {
                status_code: StatusCode::NOT_FOUND,
                ..
            })
        ),
        "{result:?}"
    );
}
