/*
 * SPDX-FileCopyrightText: Copyright (c) 2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */
/// Test against a mockup of BMC. A mockup is a directory of JSON files mirrored from a real BMC>
/// This makes for very good test for GET (e.g. get_power_state) calls, but is only a basic test
/// for POST/PATCH. For those the mockup server checks the path exists but doesn't check the body
/// values, and always returns '204 No Content'.
///
/// See tests/mockup/README for details.
use std::{
    collections::HashSet,
    env,
    path::PathBuf,
    process::{Child, Command},
    sync::Once,
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context};
use libredfish::model::{certificate::Certificate, service_root::RedfishVendor};
use libredfish::model::{ComputerSystem, ODataId};
use libredfish::{
    model::{
        resource::{IsResource, ResourceCollection},
        Manager,
    },
    Chassis, EthernetInterface, NetworkAdapter, PCIeDevice, Redfish,
};
use tracing::debug;

const ROOT_DIR: &str = env!("CARGO_MANIFEST_DIR");
const PYTHON_VENV_DIR: &str = "libredfish-python-venv";

// Ports we hope are not in use
const GENERIC_PORT: &str = "8732";
const DELL_PORT: &str = "8733";
const HPE_PORT: &str = "8734";
const LENOVO_PORT: &str = "8735";
const NVIDIA_DPU_PORT: &str = "8736";
const NVIDIA_VIKING_PORT: &str = "8737";
const SUPERMICRO_PORT: &str = "8738";
const DELL_MULTI_DPU_PORT: &str = "8739";
const NVIDIA_GH200_PORT: &str = "8740";
const NVIDIA_GB200_PORT: &str = "8741";
const NVIDIA_VERA_RUBIN_PORT: &str = "8745";
const NVIDIA_GBSWITCH_PORT: &str = "8742";
const LITEON_POWERSHELF_PORT: &str = "8743";
const DELTA_POWERSHELF_PORT: &str = "8744";
const NVIDIA_DPU_FORCE_RESET_PORT: &str = "8746";
const NVIDIA_DPU_STALE_FORCE_RESET_PORT: &str = "8747";

static SETUP: Once = Once::new();

macro_rules! test_vendor_collection_count {
    ($redfish:expr, $vendor_dir:expr, $method:ident, [$(($vendor:literal, $expected_count:literal)),+ $(,)?]) => {
        {
            $(
                if $vendor_dir == $vendor {
                    let collection = $redfish.$method().await?;
                    assert_eq!(collection.len(), $expected_count,
                        "Expected {} items for vendor {} using {}, got {}",
                        $expected_count, $vendor, stringify!($method), collection.len());
                }
            )+
            Ok::<(), anyhow::Error>(())
        }
    };
}

#[tokio::test]
async fn test_dell() -> Result<(), anyhow::Error> {
    run_integration_test("dell", DELL_PORT).await
}

#[tokio::test]
async fn test_dell_multi_dpu() -> Result<(), anyhow::Error> {
    run_integration_test("dell_multi_dpu", DELL_MULTI_DPU_PORT).await
}

#[tokio::test]
async fn test_hpe() -> Result<(), anyhow::Error> {
    run_integration_test("hpe", HPE_PORT).await
}

#[tokio::test]
async fn test_lenovo() -> Result<(), anyhow::Error> {
    run_integration_test("lenovo", LENOVO_PORT).await
}

#[tokio::test]
async fn test_nvidia_dpu() -> Result<(), anyhow::Error> {
    run_integration_test("nvidia_dpu", NVIDIA_DPU_PORT).await
}

#[tokio::test]
async fn test_nvidia_viking() -> Result<(), anyhow::Error> {
    run_integration_test("nvidia_viking", NVIDIA_VIKING_PORT).await
}

#[tokio::test]
async fn test_supermicro() -> Result<(), anyhow::Error> {
    run_integration_test("supermicro", SUPERMICRO_PORT).await
}

#[tokio::test]
async fn test_nvidia_gb200() -> Result<(), anyhow::Error> {
    run_integration_test("nvidia_gb200", NVIDIA_GB200_PORT).await
}

#[tokio::test]
#[ignore = "VR redfish not available"]
async fn test_nvidia_vera_rubin() -> Result<(), anyhow::Error> {
    run_integration_test("nvidia_vera_rubin", NVIDIA_VERA_RUBIN_PORT).await
}

#[tokio::test]
async fn test_nvidia_gbswitch() -> Result<(), anyhow::Error> {
    run_integration_test("nvidia_gbswitch", NVIDIA_GBSWITCH_PORT).await
}

#[tokio::test]
async fn test_nvidia_gh200() -> Result<(), anyhow::Error> {
    run_integration_test("nvidia_gh200", NVIDIA_GH200_PORT).await
}

#[tokio::test]
async fn test_forbidden_error_handling() -> anyhow::Result<()> {
    let _mockup_server = run_mockup_server("forbidden", GENERIC_PORT); // stops on drop

    let endpoint = libredfish::Endpoint {
        host: format!("127.0.0.1:{GENERIC_PORT}"),
        ..Default::default()
    };

    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()?;
    let redfish = pool.create_standard_client(endpoint)?;

    match redfish.get_chassis_all().await {
        Ok(_) => panic!("Request should have failed with password change required"),
        Err(libredfish::RedfishError::PasswordChangeRequired { .. }) => {} // what we want
        Err(err) => panic!("Unexpected error response: {}", err),
    }

    match redfish.get_systems().await {
        Ok(_) => panic!("Request should have failed with an HTTP error code"),
        Err(libredfish::RedfishError::HTTPErrorCode { status_code, .. }) => {
            assert_eq!(status_code, 403, "Response status code should be forbidden");
        }
        Err(err) => panic!("Unexpected error response: {}", err),
    }

    Ok(())
}

#[tokio::test]
async fn test_liteon_powershelf() -> Result<(), anyhow::Error> {
    run_integration_test("liteon_powershelf", LITEON_POWERSHELF_PORT).await
}

#[tokio::test]
async fn test_delta_powershelf() -> Result<(), anyhow::Error> {
    run_integration_test("delta_powershelf", DELTA_POWERSHELF_PORT).await
}

#[tokio::test]
async fn test_nvidia_dpu_force_reset() -> Result<(), anyhow::Error> {
    let _mockup_server =
        match run_mockup_server("nvidia_dpu_force_reset", NVIDIA_DPU_FORCE_RESET_PORT) {
            Ok(server) => server,
            Err(error) => {
                tracing::info!("Skipping integration test, env error {error}");
                return Ok(());
            }
        };
    let endpoint = libredfish::Endpoint {
        host: format!("127.0.0.1:{NVIDIA_DPU_FORCE_RESET_PORT}"),
        ..Default::default()
    };
    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()?;
    let redfish = pool.create_client(endpoint).await?;

    redfish
        .power(libredfish::SystemPowerControl::ForceRestart)
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_nvidia_dpu_stale_force_reset_falls_back() -> Result<(), anyhow::Error> {
    let _mockup_server = match run_mockup_server(
        "nvidia_dpu_stale_force_reset",
        NVIDIA_DPU_STALE_FORCE_RESET_PORT,
    ) {
        Ok(server) => server,
        Err(error) => {
            tracing::info!("Skipping integration test, env error {error}");
            return Ok(());
        }
    };
    let endpoint = libredfish::Endpoint {
        host: format!("127.0.0.1:{NVIDIA_DPU_STALE_FORCE_RESET_PORT}"),
        ..Default::default()
    };
    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()?;
    let redfish = pool.create_client(endpoint).await?;

    redfish
        .power(libredfish::SystemPowerControl::ForceRestart)
        .await?;

    Ok(())
}

async fn nvidia_dpu_integration_test(redfish: &dyn Redfish) -> Result<(), anyhow::Error> {
    let vendor = redfish.get_service_root().await?.vendor;
    assert!(vendor.is_some() && vendor.unwrap() == "Nvidia");
    let sw_inventories = redfish.get_software_inventories().await?;
    assert!(redfish
        .get_firmware(&sw_inventories[0])
        .await?
        .version
        .is_some());
    let boot = redfish.get_system().await?.boot;
    let mut boot_array = boot.boot_order;
    assert!(boot_array.len() > 1);
    boot_array.swap(0, 1);
    redfish.change_boot_order(boot_array).await?;

    let system = redfish.get_system().await?;
    assert_ne!(system.serial_number, None);

    let manager_eth_interfaces = redfish.get_manager_ethernet_interfaces().await?;
    assert!(!manager_eth_interfaces.is_empty());
    assert!(redfish
        .get_manager_ethernet_interface(&manager_eth_interfaces[0])
        .await?
        .mac_address
        .is_some());

    let chassis = redfish.get_chassis_all().await?;
    assert!(!chassis.is_empty());
    assert!(redfish.get_chassis(&chassis[0]).await?.name.is_some());

    let network_adapters = redfish.get_chassis_network_adapters(&chassis[0]).await?;
    let ports = redfish.get_ports(&chassis[0], &network_adapters[0]).await?;
    assert!(!ports.is_empty());
    assert!(redfish
        .get_port(&chassis[0], &network_adapters[0], &ports[0])
        .await?
        .current_speed_gbps
        .is_some());

    let netdev_funcs = redfish.get_network_device_functions(&chassis[0]).await?;
    assert!(!netdev_funcs.is_empty());
    assert!(redfish
        .get_network_device_function(&chassis[0], &netdev_funcs[0], None)
        .await?
        .ethernet
        .and_then(|ethernet| ethernet.mac_address)
        .is_some());

    assert_ne!(chassis.iter().find(|&x| *x == "Card1"), None);
    let chassis = redfish.get_chassis("Card1").await?;
    assert_ne!(chassis.serial_number, None);

    assert_eq!(
        chassis.serial_number.as_ref().unwrap().trim(),
        system.serial_number.as_ref().unwrap().trim()
    );

    // BlueField-3 has no chassis whose ID matches the ComputerSystem ID, so
    // ForceRestart must retain the standard ComputerSystem.Reset fallback.
    redfish
        .power(libredfish::SystemPowerControl::ForceRestart)
        .await?;

    let certificates = redfish.get_secure_boot_certificates("db").await?;
    assert!(!certificates.is_empty());
    let certificate: Certificate = redfish.get_secure_boot_certificate("db", "1").await?;
    assert!(certificate
        .issuer
        .get("CommonName")
        .is_some_and(|x| x.as_str().unwrap().contains("NVIDIA BlueField")));

    redfish.reset_bios().await?;

    Ok(())
}

fn run_mockup_server(vendor_dir: &'static str, port: &'static str) -> anyhow::Result<MockupServer> {
    SETUP.call_once(move || {
        use tracing_subscriber::fmt::Layer;
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::{filter::LevelFilter, EnvFilter};
        tracing_subscriber::registry()
            .with(
                EnvFilter::builder()
                    .with_default_directive(LevelFilter::INFO.into())
                    .from_env_lossy()
                    .add_directive("hyper=warn".parse().unwrap())
                    .add_directive("reqwest=warn".parse().unwrap())
                    .add_directive("rustls=warn".parse().unwrap()),
            )
            .with(
                Layer::default()
                    .compact()
                    .with_file(true)
                    .with_line_number(true)
                    .with_ansi(false),
            )
            .init();
        match create_python_venv() {
            Ok(pip) => {
                if let Err(e) = install_python_requirements(pip) {
                    tracing::info!("failed to install python requirements {e}")
                };
            }
            Err(e) => {
                tracing::info!("failed to create python venv {e}")
            }
        }
    });
    test_python_venv()?;
    let python = env::temp_dir()
        .join(PYTHON_VENV_DIR)
        .join("bin")
        .join("python");
    let mut mockup_server = MockupServer {
        vendor_dir,
        port,
        python,
        process: None,
    };
    mockup_server.start()?; // stops on drop
    Ok(mockup_server)
}

async fn run_integration_test(
    vendor_dir: &'static str,
    port: &'static str,
) -> Result<(), anyhow::Error> {
    let _mockup_server = match run_mockup_server(vendor_dir, port) {
        // stops on drop
        Ok(x) => x,
        Err(e) => {
            tracing::info!("Skipping integration tests, env error {e}");
            return Ok(());
        }
    };

    let endpoint = libredfish::Endpoint {
        host: format!("127.0.0.1:{port}"),
        ..Default::default()
    };

    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()?;
    // Delta power shelves advertise no vendor in the service root and expose
    // no `/Systems`, so anonymous auto-detection would fall back to the
    // standard client and fail. Force the vendor like a real (authenticated)
    // caller would via `create_client_with_vendor`.
    let redfish = if vendor_dir == "delta_powershelf" {
        pool.create_client_with_vendor(endpoint, RedfishVendor::DeltaPowerShelf, Vec::new())
            .await?
    } else {
        pool.create_client(endpoint).await?
    };

    if vendor_dir == "nvidia_dpu" {
        return nvidia_dpu_integration_test(redfish.as_ref()).await;
    }

    // Inspect the system
    let _system = redfish.get_system().await?;

    let mut all_macs = HashSet::new();
    let manager_eth_interfaces = redfish.get_manager_ethernet_interfaces().await?;
    assert!(!manager_eth_interfaces.is_empty());
    let mut manager_eth_interface_states = Vec::new();
    for iface in &manager_eth_interfaces {
        let state = redfish.get_manager_ethernet_interface(iface).await?;
        let mac = state.mac_address.clone().unwrap();
        if !all_macs.insert(mac.clone()) {
            panic!("Duplicate MAC address {} on interface {}", mac, iface);
        }
        manager_eth_interface_states.push(state);
    }

    if vendor_dir != "nvidia_gh200"
        && vendor_dir != "nvidia_gb200"
        && vendor_dir != "nvidia_vera_rubin"
        && vendor_dir != "nvidia_gbswitch"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        let system_eth_interfaces = redfish.get_system_ethernet_interfaces().await?;
        assert!(!system_eth_interfaces.is_empty());
        let mut system_eth_interface_states: Vec<libredfish::EthernetInterface> = Vec::new();
        for iface in &system_eth_interfaces {
            let state = redfish.get_system_ethernet_interface(iface).await?;
            let mac = state.mac_address.clone().unwrap();
            if !all_macs.insert(mac.clone()) {
                panic!("Duplicate MAC address {} on interface {}", mac, iface);
            }
            system_eth_interface_states.push(state);
        }
    }

    let chassis = redfish.get_chassis_all().await?;
    assert!(!chassis.is_empty());
    let mut tested_lenovo_adapter_ports = vendor_dir != "lenovo";
    for chassis_id in &chassis {
        let _chassis = redfish.get_chassis(chassis_id).await?;
        let Ok(chassis_net_adapters) = redfish.get_chassis_network_adapters(chassis_id).await
        else {
            continue;
        };
        for net_adapter_id in &chassis_net_adapters {
            let _value = redfish
                .get_chassis_network_adapter(chassis_id, net_adapter_id)
                .await?;
        }

        if vendor_dir == "lenovo" {
            if let Some(adapter_id) = chassis_net_adapters
                .iter()
                .find(|adapter_id| adapter_id.as_str() == "slot-27")
            {
                let ports = redfish.get_ports(chassis_id, adapter_id).await?;
                assert_eq!(ports, ["1", "2", "3", "4"]);

                let port = redfish.get_port(chassis_id, adapter_id, &ports[0]).await?;
                assert_eq!(port.mac_addresses()?, ["00:62:0b:4c:28:4e".parse()?]);
                tested_lenovo_adapter_ports = true;
            }
        }

        if vendor_dir == "hpe" {
            let adapter_ids = redfish.get_base_network_adapters(chassis_id).await?;
            assert!(!adapter_ids.is_empty());
            for adapter_id in &adapter_ids {
                redfish
                    .get_base_network_adapter(chassis_id, adapter_id)
                    .await?;
            }
        }
    }
    assert!(tested_lenovo_adapter_ports);

    if vendor_dir != "liteon_powershelf" && vendor_dir != "delta_powershelf" {
        assert_eq!(redfish.get_power_state().await?, libredfish::PowerState::On);
    }
    if vendor_dir != "nvidia_gbswitch"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        assert!(redfish.bios().await?.len() > 8);
    }

    // Exercise vendor-specific BIOS reset dispatch. The mock server validates
    // that the target resource or action exists, but does not apply the reset.
    if matches!(
        vendor_dir,
        "dell"
            | "dell_multi_dpu"
            | "lenovo"
            | "supermicro"
            | "nvidia_viking"
            | "nvidia_gb200"
            | "nvidia_gh200"
    ) {
        redfish.reset_bios().await?;
    }

    // Delta power shelves expose no `/Systems` resource, so there is no
    // `ComputerSystem.Reset` action to drive system power control.
    if vendor_dir != "delta_powershelf" {
        redfish
            .power(libredfish::SystemPowerControl::GracefulShutdown)
            .await?;
        redfish
            .power(libredfish::SystemPowerControl::ForceOff)
            .await?;
        redfish.power(libredfish::SystemPowerControl::On).await?;
    }

    // A real BMC requires a reboot after every change, so pretend for accuracy.
    // Dell will 400 Bad Request if you make two consecutive changes.
    if vendor_dir != "liteon_powershelf" && vendor_dir != "delta_powershelf" {
        redfish
            .lockdown(libredfish::EnabledDisabled::Disabled)
            .await?;
    }
    if vendor_dir != "delta_powershelf" {
        redfish
            .power(libredfish::SystemPowerControl::ForceRestart)
            .await?;
    }
    if vendor_dir == "dell" {
        // we're testing against static files, so these don't change
        assert!(redfish.lockdown_status().await?.is_fully_disabled());
    }

    if vendor_dir != "nvidia_gh200"
        && vendor_dir != "nvidia_gb200"
        && vendor_dir != "nvidia_vera_rubin"
        && vendor_dir != "nvidia_gbswitch"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        redfish.setup_serial_console().await?;
        redfish
            .power(libredfish::SystemPowerControl::ForceRestart)
            .await?;
        assert!(redfish.serial_console_status().await?.is_fully_enabled());
    }

    if vendor_dir != "supermicro"
        && vendor_dir != "nvidia_gh200"
        && vendor_dir != "nvidia_gb200"
        && vendor_dir != "nvidia_vera_rubin"
        && vendor_dir != "nvidia_gbswitch"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        redfish.clear_tpm().await?;
        // The mockup includes TPM clear pending operation
        assert!(!redfish.pending().await?.is_empty());
    }
    if vendor_dir != "delta_powershelf" {
        redfish
            .power(libredfish::SystemPowerControl::ForceRestart)
            .await?;
    }

    if vendor_dir != "nvidia_gbswitch"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        redfish.boot_once(libredfish::Boot::Pxe).await?;
        redfish.boot_first(libredfish::Boot::HardDisk).await?;
    }

    if vendor_dir == "nvidia_gh200" {
        let target = libredfish::BootInterfaceRef::Mac(mac_address::MacAddress::new([
            0x58, 0xA2, 0xE1, 0xBB, 0xB1, 0x0F,
        ]));
        assert!(redfish.is_bios_setup(Some(target)).await?);
        assert!(!redfish.is_boot_order_setup(target).await?);
        assert!(redfish
            .machine_setup_status(Some(target))
            .await?
            .diffs
            .iter()
            .any(|diff| diff.key == "boot_first"));
        redfish.set_boot_order_dpu_first(target).await?;
    }

    // Exercise set_boot_override on vendors that support the bare (no URI)
    // override variant via the standard Redfish Boot block PATCH. The mockup
    // doesn't validate the PATCH body -- this just verifies the call path
    // compiles, dispatches to the right impl, and reaches a writable endpoint.
    //
    // Excluded:
    //   gbswitch + liteon: mockups don't model the boot-config endpoints
    //   dell/dell_multi_dpu: tested separately below (BIOS-attribute path)
    //   lenovo: returns NotSupported (tested separately below)
    //   hpe: returns NotSupported when http_boot_uri is absent (BIOS-attribute
    //        path via UrlBootFile is the only HPE-functional mechanism;
    //        BootSourceOverride PATCHes are rejected by iLO 6 firmware)
    if vendor_dir != "dell"
        && vendor_dir != "dell_multi_dpu"
        && vendor_dir != "lenovo"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
        && vendor_dir != "nvidia_gbswitch"
        && vendor_dir != "hpe"
    {
        // Bare override (no mode, no URI). Matches what boot_once/boot_first
        // do internally for backwards-compatible callers.
        redfish
            .set_boot_override(libredfish::BootOverride {
                target: libredfish::BootSourceOverrideTarget::Pxe,
                enabled: libredfish::BootSourceOverrideEnabled::Once,
                mode: None,
                http_boot_uri: None,
            })
            .await?;

        // Full override with explicit UEFI mode and a pinned HTTP boot URI.
        // This is the new capability -- pinning the URL via the BMC so the host
        // doesn't have to rely on DHCP option 67.
        redfish
            .set_boot_override(libredfish::BootOverride {
                target: libredfish::BootSourceOverrideTarget::UefiHttp,
                enabled: libredfish::BootSourceOverrideEnabled::Continuous,
                mode: Some(libredfish::BootSourceOverrideMode::UEFI),
                http_boot_uri: Some(
                    "http://example.invalid/public/blobs/internal/x86_64/ipxe.efi".to_string(),
                ),
            })
            .await?;
    }

    // HPE uses the UrlBootFile BIOS-attribute path. The mockup accepts the
    // PATCH (default 204 No Content), and the impl returns Ok(None) since HPE
    // doesn't surface a job ID for BIOS attribute changes. Bare override (no
    // URI) returns NotSupported on HPE -- we only test the URI-supplied path.
    if vendor_dir == "hpe" {
        redfish
            .set_boot_override(libredfish::BootOverride {
                target: libredfish::BootSourceOverrideTarget::UefiHttp,
                enabled: libredfish::BootSourceOverrideEnabled::Continuous,
                mode: Some(libredfish::BootSourceOverrideMode::UEFI),
                http_boot_uri: Some(
                    "http://example.invalid/public/blobs/internal/x86_64/ipxe.efi".to_string(),
                ),
            })
            .await?;
    }

    // Dell mockups use the patch_response.json side-file mechanism in the
    // Python mockup server to simulate real iDRAC responses to PATCH
    // /Bios/Settings:
    //   - `dell` mockup: returns 202 + Location header → impl parses the
    //     job ID out of Location and returns Ok(Some(job_id)). Exercises
    //     the success path.
    //   - `dell_multi_dpu` mockup: returns 400 with the Dell-specific SYS410
    //     MessageId in the body → impl translates that to NotSupported.
    //     Exercises the read-only-attribute failure path.
    if vendor_dir == "dell" {
        match redfish
            .set_boot_override(libredfish::BootOverride {
                target: libredfish::BootSourceOverrideTarget::UefiHttp,
                enabled: libredfish::BootSourceOverrideEnabled::Continuous,
                mode: Some(libredfish::BootSourceOverrideMode::UEFI),
                http_boot_uri: Some("http://example.invalid/ipxe.efi".to_string()),
            })
            .await
        {
            Ok(Some(job_id)) => {
                assert_eq!(
                    job_id, "JID_900000000001",
                    "Expected job ID parsed from mockup's Location header"
                );
            }
            other => panic!(
                "Expected Ok(Some(job_id)) for {vendor_dir}, got {:?}",
                other.map(Some)
            ),
        }
    }

    // dell_multi_dpu (mockup-simulated SYS410 lock) and lenovo (no impl)
    // both return NotSupported.
    if vendor_dir == "dell_multi_dpu" || vendor_dir == "lenovo" {
        match redfish
            .set_boot_override(libredfish::BootOverride {
                target: libredfish::BootSourceOverrideTarget::UefiHttp,
                enabled: libredfish::BootSourceOverrideEnabled::Continuous,
                mode: Some(libredfish::BootSourceOverrideMode::UEFI),
                http_boot_uri: Some("http://example.invalid/ipxe.efi".to_string()),
            })
            .await
        {
            Err(libredfish::RedfishError::NotSupported(_)) => {}
            other => panic!(
                "Expected NotSupported for {vendor_dir}, got {:?}",
                other.map(|_| ())
            ),
        }
    }

    if vendor_dir != "delta_powershelf" {
        redfish
            .power(libredfish::SystemPowerControl::ForceRestart)
            .await?;
    }

    if vendor_dir != "liteon_powershelf" && vendor_dir != "delta_powershelf" {
        redfish
            .lockdown(libredfish::EnabledDisabled::Enabled)
            .await?;
    }

    if vendor_dir != "delta_powershelf" {
        redfish
            .power(libredfish::SystemPowerControl::GracefulRestart)
            .await?;
    }
    if vendor_dir == "lenovo" {
        assert!(redfish.lockdown_status().await?.is_fully_enabled());
    }
    if vendor_dir != "nvidia_gh200"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        let tm = redfish.get_thermal_metrics().await?;
        if vendor_dir == "nvidia_gb200" || vendor_dir == "nvidia_vera_rubin" {
            assert!(tm.leak_detectors.is_some());
        }
        if vendor_dir != "nvidia_gbswitch" {
            _ = redfish.get_power_metrics().await?;
        }
    }
    if vendor_dir != "supermicro"
        && vendor_dir != "liteon_powershelf"
        && vendor_dir != "delta_powershelf"
    {
        _ = redfish.get_system_event_log().await?;
    }

    if vendor_dir == "nvidia_viking" {
        let gpus = redfish.get_gpu_sensors().await?;
        for gpu in gpus {
            for sensor in gpu.sensors {
                assert!(sensor.reading.is_some());
                assert!(sensor.reading_type.is_some());
            }
        }
    }

    if vendor_dir == "nvidia_gb200" {
        let component_int = redfish.get_component_integrities().await?;
        assert_eq!(component_int.members.len(), 11);

        let firmware = redfish
            .get_firmware_for_component("HGX_IRoT_GPU_0")
            .await
            .unwrap();
        let firmare_expected = redfish.get_firmware("HGX_FW_GPU_0").await.unwrap();
        assert_eq!(firmware.version.unwrap(), firmare_expected.version.unwrap());

        let firmware = redfish
            .get_firmware_for_component("HGX_IRoT_GPU_1")
            .await
            .unwrap();
        let firmare_expected = redfish.get_firmware("HGX_FW_GPU_1").await.unwrap();
        assert_eq!(firmware.version.unwrap(), firmare_expected.version.unwrap());

        let firmware = redfish.get_firmware_for_component("ERoT_BMC_0").await;
        assert!(firmware.is_err());

        let chassis_cbc0 = redfish.get_chassis("CBC_0").await.unwrap();
        let vendor = chassis_cbc0.oem.unwrap();
        let nvidia = vendor.nvidia.unwrap();
        assert_eq!(nvidia.chassis_physical_slot_number.unwrap(), 1);
        assert_eq!(nvidia.compute_tray_index.unwrap(), 3);
        assert_eq!(nvidia.revision_id.unwrap(), 2);
        assert_eq!(nvidia.topology_id.unwrap(), 4);
    }

    if vendor_dir == "dell" {
        let firmware = redfish.get_firmware_for_component("ERoT_BMC_0").await;
        assert!(firmware.is_err());
    }

    test_vendor_collection_count!(
        redfish,
        vendor_dir,
        get_accounts,
        [
            ("nvidia_viking", 3),
            ("dell", 16),
            ("lenovo", 1),
            ("supermicro", 2),
            ("nvidia_gb200", 4),
            ("nvidia_vera_rubin", 4),
            ("dell_multi_dpu", 16),
            ("hpe", 2),
        ]
    )?;

    test_vendor_collection_count!(
        redfish,
        vendor_dir,
        get_drives_metrics,
        [
            ("nvidia_viking", 0), // drives are not stored properly
            ("dell", 3),
            ("lenovo", 4),
            ("supermicro", 8),
            ("nvidia_gb200", 9),
            ("nvidia_vera_rubin", 9),
            ("dell_multi_dpu", 2),
            ("hpe", 18),
        ]
    )?;

    test_vendor_collection_count!(
        redfish,
        vendor_dir,
        pcie_devices,
        [
            ("nvidia_viking", 12),
            ("dell", 13),
            ("lenovo", 15),
            ("supermicro", 26),
            ("nvidia_gb200", 0), // have no pcie devices
            ("nvidia_vera_rubin", 0),
            ("dell_multi_dpu", 10),
            ("hpe", 6),
        ]
    )?;
    if vendor_dir != "liteon_powershelf" && vendor_dir != "delta_powershelf" {
        resource_tests(redfish.as_ref()).await?;
    }

    Ok(())
}

async fn resource_tests(redfish: &dyn Redfish) -> Result<(), anyhow::Error> {
    #[allow(clippy::enum_variant_names)]
    pub enum UriType {
        ODataId(ODataId),
        OptionODataId(Option<ODataId>),
    }
    fn verify_collection<T: serde::de::DeserializeOwned + IsResource>(
        col: &ResourceCollection<T>,
        vendor: RedfishVendor,
    ) {
        assert_eq!(
            col.count as usize - col.failed_to_deserialize_count as usize,
            col.members.len()
        );
        let collection_type = col
            .odata
            .clone()
            .odata_type
            .split(".")
            .last()
            .unwrap_or_default()
            .replace("Collection", "");
        for m in &col.members {
            let member_odata_type = m.odata_type();
            let member_odata_type = member_odata_type
                .split(".")
                .last()
                .unwrap_or("unknown-type");
            // viking's mockup data contains some chassis w.o @odata.type, until we clean up mockup data we
            // need to bypass that case
            if member_odata_type.is_empty() && vendor == RedfishVendor::AMI {
                continue;
            }
            assert_eq!(collection_type, member_odata_type);
        }
    }
    async fn test_type<T>(
        redfish: &dyn Redfish,
        uri: UriType,
        vendor: RedfishVendor,
    ) -> Result<ResourceCollection<T>, anyhow::Error>
    where
        T: serde::de::DeserializeOwned + IsResource,
    {
        let id: ODataId = match uri {
            UriType::ODataId(x) => x,
            UriType::OptionODataId(x) => match x {
                Some(x) => x,
                None => return Err(anyhow!("Uri is none Option<ODataId>")),
            },
        };

        match redfish.get_collection(id).await.and_then(|c| c.try_get()) {
            Ok(x) => {
                verify_collection(&x, vendor);
                Ok(x)
            }
            Err(e) => Err(anyhow!(e.to_string())),
        }
    }

    let service_root = redfish.get_service_root().await?;
    assert!(service_root.vendor().is_some());
    let vendor = service_root.vendor().unwrap();
    let _managers_rc = test_type::<Manager>(
        redfish,
        UriType::OptionODataId(service_root.managers.clone()),
        vendor,
    )
    .await?;
    let chassis_rc = test_type::<Chassis>(
        redfish,
        UriType::OptionODataId(service_root.chassis.clone()),
        vendor,
    )
    .await?;
    let _systems_rc = test_type::<ComputerSystem>(
        redfish,
        UriType::OptionODataId(service_root.systems.clone()),
        vendor,
    )
    .await?;

    let chassis_id = match vendor {
        RedfishVendor::Lenovo | RedfishVendor::Supermicro | RedfishVendor::Hpe => "1",
        RedfishVendor::LenovoAMI => "Self",
        RedfishVendor::AMI => "DGX",
        RedfishVendor::NvidiaDpu => "Card1",
        RedfishVendor::Dell => "System.Embedded.1",
        RedfishVendor::P3809 => {
            let mut result = "BMC_0";
            for x in chassis_rc.members.clone() {
                if x.id.unwrap_or_default().contains("MGX_NVSwitch_0") {
                    result = "MGX_NVSwitch_0";
                    break;
                }
            }
            result
        }
        RedfishVendor::NvidiaGH200 => "BMC_0",
        RedfishVendor::NvidiaGBx00 | RedfishVendor::VeraRubin => "Chassis_0", // this is not the catch-all chassis id, gb200 redfish is not structured to aggregate into one chassis id
        RedfishVendor::NvidiaGBSwitch => "MGX_NVSwitch_0",
        _ => return Err(anyhow!("Unknown vendor could not identify chassis")),
    };
    if vendor != RedfishVendor::NvidiaDpu {
        let ch = match chassis_rc
            .members
            .clone()
            .into_iter()
            .find(|c| c.id.clone().unwrap_or_default() == chassis_id)
        {
            Some(x) => x,
            None => return Err(anyhow!("Chassis with id {} not found", chassis_id)),
        };

        if let Some(pcie_devs_oid) = ch.pcie_devices.as_ref() {
            debug!("Testing pcie_devices");
            let _pcie_devs_rc = test_type::<PCIeDevice>(
                redfish,
                UriType::ODataId(pcie_devs_oid.to_owned()),
                vendor,
            )
            .await?;
        }

        if let Some(nw_adapters_oid) = ch.network_adapters.as_ref() {
            debug!("Testing network_adapters");
            let _nw_adapter_rc = test_type::<NetworkAdapter>(
                redfish,
                UriType::ODataId(nw_adapters_oid.to_owned()),
                vendor,
            )
            .await?;
        }

        let sys = redfish.get_system().await?;
        let sys2 = redfish
            .get_resource(sys.odata.odata_id.into())
            .await
            .and_then(|t| t.try_get::<ComputerSystem>())?;

        assert_eq!(sys.model.as_ref(), sys2.model.as_ref());
        assert_eq!(sys.id, sys2.id);

        if let Some(sys_ethernet_interfaces_id) = sys.ethernet_interfaces.as_ref() {
            debug!("Testing system.ethernet_interfaces");
            let nw_ethernet_rc = test_type::<EthernetInterface>(
                redfish,
                UriType::ODataId(sys_ethernet_interfaces_id.to_owned()),
                vendor,
            )
            .await?;
            debug!("{} ethernet_interfaces found", nw_ethernet_rc.count);
        }
    }
    Ok(())
}

fn test_python_venv() -> Result<(), anyhow::Error> {
    let venv_dir = get_tmp_dir();
    let venv_out = Command::new("python3")
        .arg("-m")
        .arg("venv")
        .arg(&venv_dir)
        .output()
        .context("Is 'python3' on your $PATH?")?;
    if !venv_out.status.success() {
        eprintln!("*** Python virtual env creation failed:");
        eprintln!("\tSTDOUT: {}", String::from_utf8_lossy(&venv_out.stdout));
        eprintln!("\tSTDERR: {}", String::from_utf8_lossy(&venv_out.stderr));
        std::fs::remove_dir_all(venv_dir.clone())?;
        return Err(anyhow!(
            "Failed running 'python3 -m venv {}. Exit code {}",
            venv_dir.clone().display(),
            venv_out.status.code().unwrap_or(-1),
        ));
    }

    std::fs::remove_dir_all(venv_dir)?;
    Ok(())
}

/// Create a python virtualenv to install our requirements into.
/// Return the path of pip
fn create_python_venv() -> Result<PathBuf, anyhow::Error> {
    let venv_dir = env::temp_dir().join(PYTHON_VENV_DIR);
    let venv_out = Command::new("python3")
        .arg("-m")
        .arg("venv")
        .arg(&venv_dir)
        .output()
        .context("Is 'python3' on your $PATH?")?;
    if !venv_out.status.success() {
        eprintln!("*** Python virtual env creation failed:");
        eprintln!("\tSTDOUT: {}", String::from_utf8_lossy(&venv_out.stdout));
        eprintln!("\tSTDERR: {}", String::from_utf8_lossy(&venv_out.stderr));
        return Err(anyhow!(
            "Failed running 'python3 -m venv {}. Exit code {}",
            venv_dir.display(),
            venv_out.status.code().unwrap_or(-1),
        ));
    }

    Ok(venv_dir.join("bin/pip"))
}

fn install_python_requirements(pip: PathBuf) -> Result<(), anyhow::Error> {
    let req_path = PathBuf::from(ROOT_DIR)
        .join("tests")
        .join("requirements.txt");
    let output = Command::new(&pip)
        .arg("install")
        .arg("-q")
        .arg("--requirement")
        .arg(&req_path)
        .output()?;
    if !output.status.success() {
        eprintln!("*** pip install failed:");
        eprintln!("\tSTDOUT: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("\tSTDERR: {}", String::from_utf8_lossy(&output.stderr));
        return Err(anyhow!(
            "Failed running '{} install -q --requirement {}. Exit code {}",
            pip.display(),
            req_path.display(),
            output.status.code().unwrap_or(-1),
        ));
    }
    Ok(())
}

struct MockupServer {
    vendor_dir: &'static str,
    port: &'static str,
    python: PathBuf,

    process: Option<Child>,
}

impl Drop for MockupServer {
    fn drop(&mut self) {
        if self.process.is_none() {
            return;
        }
        self.process.take().unwrap().kill().unwrap();
        sleep(Duration::from_secs(1)); // let it stop
    }
}

impl MockupServer {
    fn start(&mut self) -> Result<(), anyhow::Error> {
        // For extra debugging edit redfishMockupServer.py change the log level at the top
        self.process = Some(
            Command::new(&self.python)
                .current_dir(PathBuf::from(ROOT_DIR).join("tests"))
                .arg("redfishMockupServer.py")
                .arg("--port")
                .arg(self.port)
                .arg("--dir")
                .arg(format!("mockups/{}/", self.vendor_dir))
                .arg("--ssl")
                .arg("--cert")
                .arg("cert.pem")
                .arg("--key")
                .arg("key.pem")
                .spawn()?,
        );
        sleep(Duration::from_secs(1)); // let it start
        Ok(())
    }
}

fn get_tmp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = format!("{}-{}-{}", PYTHON_VENV_DIR, std::process::id(), nanos);
    env::temp_dir().join(&temp_dir)
}
