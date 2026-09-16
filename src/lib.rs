/*
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
use std::{collections::HashMap, fmt, future::Future, path::Path, pin::Pin, time::Duration};

pub mod model;
use model::account_service::ManagerAccount;
pub use model::boot::{
    BootOverride, BootSourceOverrideEnabled, BootSourceOverrideMode, BootSourceOverrideTarget,
};
pub use model::chassis::{Assembly, Chassis, NetworkAdapter};
pub use model::ethernet_interface::EthernetInterface;
pub use model::manager::ManagerResetType;
pub use model::network_device_function::NetworkDeviceFunction;
use model::oem::nvidia_dpu::{HostPrivilegeLevel, InternalCPUModel, NicMode};
pub use model::port::NetworkPort;
pub use model::resource::{Collection, OData, Resource};
use model::sensor::GPUSensors;
use model::service_root::{RedfishVendor, ServiceRoot};
use model::software_inventory::SoftwareInventory;
pub use model::system::{BootOptions, PCIeDevice, PowerState, SystemPowerControl, Systems};
use model::task::Task;
use model::update_service::{ComponentType, TransferProtocolType, UpdateService};
pub use model::EnabledDisabled;
use model::Manager;
use model::{secure_boot::SecureBoot, BootOption, ComputerSystem, ODataId};
use serde::{Deserialize, Serialize};
mod ami;
mod dell;
mod error;
mod giga_computing_ami;
mod hpe;
pub mod jsonmap;
mod lenovo;

mod delta_powershelf;
mod liteon_powershelf;
mod network;
mod nvidia_dpu;

mod nvidia_gbswitch;
mod nvidia_gbx00;
mod nvidia_gh200;
mod nvidia_vera_rubin;
mod nvidia_viking;
mod supermicro;
mod sushy;
pub use network::{Endpoint, RedfishClientPool, RedfishClientPoolBuilder, REDFISH_ENDPOINT};
pub mod standard;
pub use error::RedfishError;

/// Reexported of reqwest for types needed in
/// RedfishClientPoolBuilder.
pub use reqwest;

use crate::model::certificate::Certificate;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::power::Power;
use crate::model::sel::LogEntry;
use crate::model::storage::Drives;
use crate::model::thermal::Thermal;

pub type RedfishFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Interface to a BMC Redfish server. All calls will include one or more HTTP network calls.
pub trait Redfish: Send + Sync + 'static {
    /// Returns the standard Redfish implementation used for default behavior.
    fn std_redfish(&self) -> &standard::RedfishStandard;

    /// Rename a user
    fn change_username<'a>(
        &'a self,
        old_name: &'a str,
        new_name: &'a str,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::change_username(
            self.std_redfish(),
            old_name,
            new_name,
        )
    }

    /// Change password by username
    /// This looks up the ID for given username before calling change_password_by_id.
    /// That lookup makes it unsuitable for changing the initial password on
    /// PasswordChangeRequired.
    fn change_password<'a>(
        &'a self,
        username: &'a str,
        new_pass: &'a str,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::change_password(
            self.std_redfish(),
            username,
            new_pass,
        )
    }

    /// Change password by id
    fn change_password_by_id<'a>(
        &'a self,
        account_id: &'a str,
        new_pass: &'a str,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::change_password_by_id(
            self.std_redfish(),
            account_id,
            new_pass,
        )
    }

    /// List current user accounts
    fn get_accounts<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<ManagerAccount>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_accounts(self.std_redfish())
    }

    /// Create a new user
    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        role_id: RoleId,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::create_user(
            self.std_redfish(),
            username,
            password,
            role_id,
        )
    }

    /// Delete a BMC user
    fn delete_user<'a>(&'a self, username: &'a str) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::delete_user(self.std_redfish(), username)
    }

    // Get firmware version for particular firmware inventory id
    fn get_firmware<'a>(
        &'a self,
        id: &'a str,
    ) -> RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_firmware(self.std_redfish(), id)
    }

    // Get software inventory collection
    fn get_software_inventories<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_software_inventories(self.std_redfish())
    }

    // List all Tasks
    fn get_tasks<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_tasks(self.std_redfish())
    }

    // Get information about a task
    fn get_task<'a>(&'a self, id: &'a str) -> RedfishFuture<'a, Result<Task, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_task(self.std_redfish(), id)
    }

    /// Is this thing even on?
    fn get_power_state<'a>(&'a self) -> RedfishFuture<'a, Result<PowerState, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_power_state(self.std_redfish())
    }

    /// Returns info about operations that the service supports.
    fn get_service_root<'a>(&'a self) -> RedfishFuture<'a, Result<ServiceRoot, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_service_root(self.std_redfish())
    }

    /// Returns info about available computer systems.
    fn get_systems<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_systems(self.std_redfish())
    }

    /// Returns info about computer system.
    fn get_system<'a>(&'a self) -> RedfishFuture<'a, Result<ComputerSystem, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_system(self.std_redfish())
    }

    /// Returns info about available managers.
    fn get_managers<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_managers(self.std_redfish())
    }

    /// Returns info about managers
    fn get_manager<'a>(&'a self) -> RedfishFuture<'a, Result<Manager, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_manager(self.std_redfish())
    }

    /// Get Secure Boot state
    fn get_secure_boot<'a>(&'a self) -> RedfishFuture<'a, Result<SecureBoot, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_secure_boot(self.std_redfish())
    }

    /// Disables Secure Boot
    fn disable_secure_boot<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::disable_secure_boot(self.std_redfish())
    }

    /// Enables Secure Boot
    fn enable_secure_boot<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::enable_secure_boot(self.std_redfish())
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        database_id: &'a str,
        certificate_id: &'a str,
    ) -> RedfishFuture<'a, Result<Certificate, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_secure_boot_certificate(
            self.std_redfish(),
            database_id,
            certificate_id,
        )
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        database_id: &'a str,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_secure_boot_certificates(
            self.std_redfish(),
            database_id,
        )
    }

    /// Adds certificate to secure boot DB
    /// database_id: "db" for database, "pk" for PK database
    /// Need to reboot DPU for UEFI Redfish client to execute.
    fn add_secure_boot_certificate<'a>(
        &'a self,
        pem_cert: &'a str,
        database_id: &'a str,
    ) -> RedfishFuture<'a, Result<Task, RedfishError>> {
        <standard::RedfishStandard as Redfish>::add_secure_boot_certificate(
            self.std_redfish(),
            pem_cert,
            database_id,
        )
    }

    /// Power supplies and voltages metrics
    fn get_power_metrics<'a>(&'a self) -> RedfishFuture<'a, Result<Power, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_power_metrics(self.std_redfish())
    }

    /// Change power state: on, off, reboot, etc
    fn power<'a>(
        &'a self,
        action: SystemPowerControl,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::power(self.std_redfish(), action)
    }

    /// Reboot the BMC itself. `reset_type` selects the Redfish `Manager.Reset`
    /// action; `None` uses the vendor's default (`GracefulRestart` for the
    /// standard implementation, `ForceRestart` for vendors that only support
    /// it, e.g. AMI and Viking).
    fn bmc_reset<'a>(
        &'a self,
        reset_type: Option<ManagerResetType>,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::bmc_reset(self.std_redfish(), reset_type)
    }

    /// Reset Chassis
    fn chassis_reset<'a>(
        &'a self,
        chassis_id: &'a str,
        reset_type: SystemPowerControl,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::chassis_reset(
            self.std_redfish(),
            chassis_id,
            reset_type,
        )
    }

    /// Reset BMC to the factory defaults.
    fn bmc_reset_to_defaults<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::bmc_reset_to_defaults(self.std_redfish())
    }

    /// Fans and temperature sensors
    fn get_thermal_metrics<'a>(&'a self) -> RedfishFuture<'a, Result<Thermal, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_thermal_metrics(self.std_redfish())
    }

    /// Voltage, temperature, etc sensors for gpus if they exist.
    fn get_gpu_sensors<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_gpu_sensors(self.std_redfish())
    }

    /// get system event log similar to ipmitool sel
    fn get_system_event_log<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_system_event_log(self.std_redfish())
    }

    /// get bmc event log (power events, etc.)
    fn get_bmc_event_log<'a>(
        &'a self,
        from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_bmc_event_log(self.std_redfish(), from)
    }

    /// get drives metrics
    fn get_drives_metrics<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<Drives>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_drives_metrics(self.std_redfish())
    }

    /// Sets up a reasonable UEFI configuration.
    /// remember to call lockdown() afterwards to secure the server
    /// - boot_interface: identifies the NIC you wish to boot from. A
    ///   `BootInterfaceRef::Mac` uses the existing vendor lookup by MAC, while
    ///   `BootInterfaceRef::InterfaceId` uses a vendor-native Redfish
    ///   `EthernetInterface.Id`. `BootInterfaceRef::Pair` supplies both so each
    ///   vendor can use its native identifier without resolving one from the
    ///   other.
    ///   If not given we look for a Mellanox Bluefield DPU and use that.
    ///   Not applicable to Supermicro and the DPU itself.
    ///   bios_profiles: Map of vendor/model (with spaces replaced by underscores)/profile/type
    ///   to extra settings; expected to come from config rather than hardcoded.
    ///   selected_profile: Profile to use (if present)
    ///
    /// Returns Ok(Some(job_id)) when the vendor creates a job for the BIOS PATCH (e.g. Dell);
    ///
    /// Ok(None) when no job is created. Caller should wait for job completion before configuring boot order.
    fn machine_setup<'a>(
        &'a self,
        boot_interface: Option<BootInterfaceRef<'a>>,
        bios_profiles: &'a BiosProfileVendor,
        selected_profile: BiosProfileType,
        oem_manager_profiles: &'a BiosProfileVendor,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::machine_setup(
            self.std_redfish(),
            boot_interface,
            bios_profiles,
            selected_profile,
            oem_manager_profiles,
        )
    }

    /// Is everything that machine_setup does already done?
    fn machine_setup_status<'a>(
        &'a self,
        boot_interface: Option<BootInterfaceRef<'a>>,
    ) -> RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        <standard::RedfishStandard as Redfish>::machine_setup_status(
            self.std_redfish(),
            boot_interface,
        )
    }

    /// Check if only the BIOS/BMC setup is done
    fn is_bios_setup<'a>(
        &'a self,
        boot_interface: Option<BootInterfaceRef<'a>>,
    ) -> RedfishFuture<'a, Result<bool, RedfishError>> {
        <standard::RedfishStandard as Redfish>::is_bios_setup(self.std_redfish(), boot_interface)
    }

    /// Apply a standard BMC password policy. This varies a lot by vendor,
    /// but at a minimum we want passwords to never expire, because our BMCs are
    /// not actively used by humans.
    fn set_machine_password_policy<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_machine_password_policy(self.std_redfish())
    }

    /// Lock the BIOS and BMC ready for tenant use. Disabled reverses the changes.
    fn lockdown<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::lockdown(self.std_redfish(), target)
    }

    /// Are the BIOS and BMC currently locked down?
    fn lockdown_status<'a>(&'a self) -> RedfishFuture<'a, Result<Status, RedfishError>> {
        <standard::RedfishStandard as Redfish>::lockdown_status(self.std_redfish())
    }

    /// Enable SSH access to console
    fn setup_serial_console<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::setup_serial_console(self.std_redfish())
    }

    /// Is the serial console setup?
    fn serial_console_status<'a>(&'a self) -> RedfishFuture<'a, Result<Status, RedfishError>> {
        <standard::RedfishStandard as Redfish>::serial_console_status(self.std_redfish())
    }

    /// Show available boot options
    fn get_boot_options<'a>(&'a self) -> RedfishFuture<'a, Result<BootOptions, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_boot_options(self.std_redfish())
    }

    /// Show available boot options
    fn get_boot_option<'a>(
        &'a self,
        option_id: &'a str,
    ) -> RedfishFuture<'a, Result<BootOption, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_boot_option(self.std_redfish(), option_id)
    }

    /// Boot a single time of the given target. Does not change boot order after that.
    fn boot_once<'a>(&'a self, target: Boot) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::boot_once(self.std_redfish(), target)
    }

    /// Change boot order putting this target first
    fn boot_first<'a>(&'a self, target: Boot) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::boot_first(self.std_redfish(), target)
    }

    /// Set a boot source override, optionally including an HTTP boot URI.
    ///
    /// This is a lower-level alternative to [`Redfish::boot_once`] /
    /// [`Redfish::boot_first`] that exposes the full Redfish `Boot` override
    /// shape: `target`, `enabled` (`Once`/`Continuous`/`Disabled`), `mode`
    /// (`UEFI`/`Legacy`), and `http_boot_uri`.
    ///
    /// When `target` is `UefiHttp` and `http_boot_uri` is `Some`, the BMC pins
    /// the boot URL — the host will UEFI-HTTP-boot from that URI on the next
    /// applicable boot without needing DHCP option 67. If `http_boot_uri` is
    /// `None`, the firmware falls back to DHCP option 67 per the UEFI HTTP
    /// Boot specification.
    ///
    /// Returns an optional job ID. Vendors that route the change through a
    /// BIOS settings job schedule it to apply on next reboot and return the
    /// job ID. Vendors that apply the change immediately return `None`.
    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_boot_override(self.std_redfish(), settings)
    }

    /// Change boot order by setting boot array.
    fn change_boot_order<'a>(
        &'a self,
        boot_array: Vec<String>,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::change_boot_order(self.std_redfish(), boot_array)
    }

    /// Reset and enable the TPM
    fn clear_tpm<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::clear_tpm(self.std_redfish())
    }

    /// List PCIe devices
    fn pcie_devices<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<PCIeDevice>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::pcie_devices(self.std_redfish())
    }

    /// Update BMC firmware
    fn update_firmware<'a>(
        &'a self,
        filename: tokio::fs::File,
    ) -> RedfishFuture<'a, Result<Task, RedfishError>> {
        <standard::RedfishStandard as Redfish>::update_firmware(self.std_redfish(), filename)
    }

    /// Update UEFI firmware, returns a task ID
    fn update_firmware_multipart<'a>(
        &'a self,
        firmware: &'a Path,
        reboot: bool,
        timeout: Duration,
        component_type: ComponentType,
    ) -> RedfishFuture<'a, Result<String, RedfishError>> {
        <standard::RedfishStandard as Redfish>::update_firmware_multipart(
            self.std_redfish(),
            firmware,
            reboot,
            timeout,
            component_type,
        )
    }

    /// This action shall update installed software components in a software image file located at an ImageURI parameter-specified URI.
    /// image_uri - The URI of the software image to install.
    /// transfer_protocol - The network protocol that the update service uses to retrieve the software image file located at the URI provided in ImageURI.
    /// This parameter is ignored if the URI provided in ImageURI contains a scheme.
    /// targets - An array of URIs that indicate where to apply the update image.
    fn update_firmware_simple_update<'a>(
        &'a self,
        image_uri: &'a str,
        targets: Vec<String>,
        transfer_protocol: TransferProtocolType,
    ) -> RedfishFuture<'a, Result<Task, RedfishError>> {
        <standard::RedfishStandard as Redfish>::update_firmware_simple_update(
            self.std_redfish(),
            image_uri,
            targets,
            transfer_protocol,
        )
    }

    /*
     * Diagnostic calls
     */
    /// All the BIOS values for this provider. Very OEM specific.
    fn bios<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::bios(self.std_redfish())
    }

    /// Modify specific BIOS values.  Also very OEM and model specific.
    fn set_bios<'a>(
        &'a self,
        values: HashMap<String, serde_json::Value>,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_bios(self.std_redfish(), values)
    }

    /// Reset BIOS to factory settings
    fn reset_bios<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::reset_bios(self.std_redfish())
    }

    /// Pending BIOS attributes. Changes that were requested but not applied yet because
    /// they need a reboot.
    fn pending<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::pending(self.std_redfish())
    }

    /// Clear all pending jobs
    fn clear_pending<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::clear_pending(self.std_redfish())
    }

    // List all Network Device Functions of a given Chassis
    fn get_network_device_functions<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_network_device_functions(
            self.std_redfish(),
            chassis_id,
        )
    }

    // Get Network Device Function details
    fn get_network_device_function<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
        port: Option<&'a str>,
    ) -> RedfishFuture<'a, Result<NetworkDeviceFunction, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_network_device_function(
            self.std_redfish(),
            chassis_id,
            id,
            port,
        )
    }

    // List all Chassises
    fn get_chassis_all<'a>(&'a self) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_chassis_all(self.std_redfish())
    }

    // Get Chassis details
    fn get_chassis<'a>(&'a self, id: &'a str) -> RedfishFuture<'a, Result<Chassis, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_chassis(self.std_redfish(), id)
    }

    // Get Chassis Assembly details
    fn get_chassis_assembly<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> RedfishFuture<'a, Result<Assembly, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_chassis_assembly(self.std_redfish(), chassis_id)
    }

    // List all Network Adapters for the specific Chassis
    fn get_chassis_network_adapters<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_chassis_network_adapters(
            self.std_redfish(),
            chassis_id,
        )
    }

    // Get Network Adapter details for the specific Chassis and Network Adapter
    fn get_chassis_network_adapter<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
    ) -> RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_chassis_network_adapter(
            self.std_redfish(),
            chassis_id,
            id,
        )
    }

    // List all Base Network Adapters for the specific Chassis
    // Only implemented in iLO5
    fn get_base_network_adapters<'a>(
        &'a self,
        system_id: &'a str,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_base_network_adapters(
            self.std_redfish(),
            system_id,
        )
    }

    // Get Base Network Adapter details for the specific Chassis and Network Adapter
    // Only implemented in iLO5
    fn get_base_network_adapter<'a>(
        &'a self,
        system_id: &'a str,
        id: &'a str,
    ) -> RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_base_network_adapter(
            self.std_redfish(),
            system_id,
            id,
        )
    }

    // List all High Speed Ports of a given Chassis
    fn get_ports<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_ports(
            self.std_redfish(),
            chassis_id,
            network_adapter,
        )
    }

    // Get High Speed Port details
    fn get_port<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
        id: &'a str,
    ) -> RedfishFuture<'a, Result<NetworkPort, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_port(
            self.std_redfish(),
            chassis_id,
            network_adapter,
            id,
        )
    }

    // List all Ethernet Interfaces for the default `Manager`
    fn get_manager_ethernet_interfaces<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_manager_ethernet_interfaces(self.std_redfish())
    }

    // Get Ethernet Interface details for an interface on the default `Manager`
    fn get_manager_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> RedfishFuture<'a, Result<EthernetInterface, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_manager_ethernet_interface(
            self.std_redfish(),
            id,
        )
    }

    // List all Ethernet Interfaces for the default `System`
    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_system_ethernet_interfaces(self.std_redfish())
    }

    // Get Ethernet Interface details for an interface on the default `System`
    fn get_system_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> RedfishFuture<'a, Result<EthernetInterface, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_system_ethernet_interface(
            self.std_redfish(),
            id,
        )
    }

    // Change UEFI Password
    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::change_uefi_password(
            self.std_redfish(),
            current_uefi_password,
            new_uefi_password,
        )
    }

    fn get_job_state<'a>(
        &'a self,
        job_id: &'a str,
    ) -> RedfishFuture<'a, Result<JobState, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_job_state(self.std_redfish(), job_id)
    }

    /// A kind-of-generic method to retrieve any Redfish resource. A resource is a top level object defined by Redfish spec snd
    /// implements trait named IsResource. A resource should have @odata.type and @odata.id annotations as defined by the spec.
    ///
    /// Method takes OdatIaD as the input that is defined as the URI for the resource.
    ///
    /// The following two macros are provided to implement IsResource trait for objects. Use the one that mathces
    /// the struct depending on how @odata.id and @odata.type are captured. Example use of macros:
    ///
    ///  impl_is_resource_for_option_odatalinks!(crate::EthernetInterface);   # captures @odata.xxxx annotations in Option<ODataLinks>
    ///  impl_is_resource!(crate::model::PCIeDevice);                         # Uses OData instead
    ///
    ///
    /// This method returns Resource struct that contains the raw JSON and can be converted to an resource by calling try_get<T>()
    /// method. Resource::try_get<T>() method will desrialize JSON making surethat requested type T matches with @odata.type. Error will be
    /// returned otherwise. This imposes a restriction on naming struct's for resources. @odata.type has the format #<ResourceType>.<Version>.<TermName>
    /// Struct name for @odata.type should be named <TermName>. For example, @odata.type for systems is "@odata.type": "#ComputerSystem.v1_17_0.ComputerSystem".
    /// Corresponding RUST struct is named ComputerSystem.
    ///
    /// Example ussage:
    /// let chassis : Chassis =  redfish.get_resource(chassis_odata_id)
    ///                             .await
    ///                              .and_then(|r| {r.try_get()})?;
    ///
    ///
    fn get_resource<'a>(
        &'a self,
        id: ODataId,
    ) -> RedfishFuture<'a, Result<Resource, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_resource(self.std_redfish(), id)
    }

    /// A kind-of-generic api to retrieve any resource. See get_resource() api for more details.
    /// This method returns Collection object that contains raw JSON and can be conveted to
    /// generic type ResourceCollection<T> via generic method try_get()
    /// Sample usage:
    ///
    /// let rc_nw_adapter : ResourceCollection<NetworkAdapter> =  self.s.get_collection(na_id)
    ///                                                              .await
    ///                                                              .and_then(|r| r.try_get())?;
    /// try_get() will make sure that @odata.type of the returned collection matches with requested type T; error is
    /// returned otherwise.
    /// ODataId passed in should be a URI of resource collection as defined by Redfish spec. Resource collection's @odata.type
    /// ends with suffix Collection. For example, @odata.type of EthernetInfetface collection is
    ///
    ///    "#EthernetInterfaceCollection.EthernetInterfaceCollection"
    ///
    /// This collection can only be connverted to ResourceCollection<EthernetInterface>
    ///
    /// This method fetches all member objects of the collection in a single request by appending
    /// '?$expand=.($levels=1)' to the URI as defined by the spec.
    fn get_collection<'a>(
        &'a self,
        id: ODataId,
    ) -> RedfishFuture<'a, Result<Collection, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_collection(self.std_redfish(), id)
    }

    /// Change the boot order so the system will boot from the chosen NIC first.
    ///
    /// `boot_interface` selects the target NIC. A `BootInterfaceRef::Mac` uses
    /// the existing vendor lookup by MAC, while `BootInterfaceRef::InterfaceId`
    /// uses a vendor-native Redfish `EthernetInterface.Id`.
    /// `BootInterfaceRef::Pair` supplies both so the vendor can use its native
    /// identifier directly.
    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: BootInterfaceRef<'a>,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_boot_order_dpu_first(
            self.std_redfish(),
            boot_interface,
        )
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::clear_uefi_password(
            self.std_redfish(),
            current_uefi_password,
        )
    }

    fn get_update_service<'a>(&'a self) -> RedfishFuture<'a, Result<UpdateService, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_update_service(self.std_redfish())
    }

    fn get_base_mac_address<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_base_mac_address(self.std_redfish())
    }

    fn lockdown_bmc<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::lockdown_bmc(self.std_redfish(), target)
    }

    fn is_ipmi_over_lan_enabled<'a>(&'a self) -> RedfishFuture<'a, Result<bool, RedfishError>> {
        <standard::RedfishStandard as Redfish>::is_ipmi_over_lan_enabled(self.std_redfish())
    }

    fn enable_ipmi_over_lan<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::enable_ipmi_over_lan(self.std_redfish(), target)
    }

    fn enable_rshim_bmc<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::enable_rshim_bmc(self.std_redfish())
    }

    // Only applicable to Vikings
    fn clear_nvram<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::clear_nvram(self.std_redfish())
    }

    // Only applicable to DPUs
    fn get_nic_mode<'a>(&'a self) -> RedfishFuture<'a, Result<Option<NicMode>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_nic_mode(self.std_redfish())
    }

    // Only applicable to DPUs
    fn set_nic_mode<'a>(&'a self, mode: NicMode) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_nic_mode(self.std_redfish(), mode)
    }

    /// Enable infinite boot
    fn enable_infinite_boot<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::enable_infinite_boot(self.std_redfish())
    }

    /// Check if infinite boot is enabled
    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::is_infinite_boot_enabled(self.std_redfish())
    }

    // Only applicable to DPUs
    fn set_host_rshim<'a>(
        &'a self,
        enabled: EnabledDisabled,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_host_rshim(self.std_redfish(), enabled)
    }

    // Only applicable to DPUs
    fn get_host_rshim<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Option<EnabledDisabled>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_host_rshim(self.std_redfish())
    }

    // Only applicable to Dells
    fn set_idrac_lockdown<'a>(
        &'a self,
        enabled: EnabledDisabled,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_idrac_lockdown(self.std_redfish(), enabled)
    }

    // Only applicable to Dells
    fn get_boss_controller<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_boss_controller(self.std_redfish())
    }

    // Only applicable to Dells
    fn decommission_storage_controller<'a>(
        &'a self,
        controller_id: &'a str,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::decommission_storage_controller(
            self.std_redfish(),
            controller_id,
        )
    }

    // Only applicable to Dells
    fn create_storage_volume<'a>(
        &'a self,
        controller_id: &'a str,
        volume_name: &'a str,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::create_storage_volume(
            self.std_redfish(),
            controller_id,
            volume_name,
        )
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        <standard::RedfishStandard as Redfish>::ac_powercycle_supported_by_power(self.std_redfish())
    }

    /// Is the boot order already configured for `boot_interface`? See
    /// `set_boot_order_dpu_first` for the variant semantics.
    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: BootInterfaceRef<'a>,
    ) -> RedfishFuture<'a, Result<bool, RedfishError>> {
        <standard::RedfishStandard as Redfish>::is_boot_order_setup(
            self.std_redfish(),
            boot_interface,
        )
    }

    /// Returns info about component integrity
    fn get_component_integrities<'a>(
        &'a self,
    ) -> RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_component_integrities(self.std_redfish())
    }

    /// Returns info about component integrity
    fn get_firmware_for_component<'a>(
        &'a self,
        component_integrity_id: &'a str,
    ) -> RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_firmware_for_component(
            self.std_redfish(),
            component_integrity_id,
        )
    }

    /// Component/evidence apis are taking URL as of now since not sure if all vendors keep
    /// certificate and evidence in chassis/same place. Once tested with all vendors, the url can
    /// be changed into id and device parameters.
    /// Fetches component certificate
    fn get_component_ca_certificate<'a>(
        &'a self,
        url: &'a str,
    ) -> RedfishFuture<'a, Result<model::component_integrity::CaCertificate, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_component_ca_certificate(
            self.std_redfish(),
            url,
        )
    }

    /// Trigger evidence collection
    fn trigger_evidence_collection<'a>(
        &'a self,
        url: &'a str,
        nonce: &'a str,
    ) -> RedfishFuture<'a, Result<Task, RedfishError>> {
        <standard::RedfishStandard as Redfish>::trigger_evidence_collection(
            self.std_redfish(),
            url,
            nonce,
        )
    }

    /// Fetches component certificate
    fn get_evidence<'a>(
        &'a self,
        url: &'a str,
    ) -> RedfishFuture<'a, Result<model::component_integrity::Evidence, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_evidence(self.std_redfish(), url)
    }

    // Sets the host privilege level for a DPU
    fn set_host_privilege_level<'a>(
        &'a self,
        level: HostPrivilegeLevel,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_host_privilege_level(self.std_redfish(), level)
    }

    // Sets the timezone to UTC
    // Only applicable to Dells
    fn set_utc_timezone<'a>(&'a self) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_utc_timezone(self.std_redfish())
    }

    // Gets Oem.Nvidia.EastWestControlEnabled from a single CX NIC Settings
    // Only applicable to Vera-Rubin. `nic_index` must be in 0..8.
    fn get_spx_nic_east_west_control_enabled<'a>(
        &'a self,
        nic_index: u8,
    ) -> RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_spx_nic_east_west_control_enabled(
            self.std_redfish(),
            nic_index,
        )
    }

    // Sets Oem.Nvidia.EastWestControlEnabled on a single CX NIC Settings
    // Only applicable to Vera-Rubin. `nic_index` must be in 0..8.
    fn set_spx_nic_east_west_control_enabled<'a>(
        &'a self,
        nic_index: u8,
        enabled: bool,
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_spx_nic_east_west_control_enabled(
            self.std_redfish(),
            nic_index,
            enabled,
        )
    }

    // Gets MAC address for a single CX_NIC_{nic_index}_Port_0 EthernetInterface
    // Only applicable to Vera-Rubin. `nic_index` must be in 0..8.
    fn get_spx_nic_mac_address<'a>(
        &'a self,
        nic_index: u8,
    ) -> RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_spx_nic_mac_address(
            self.std_redfish(),
            nic_index,
        )
    }

    // Gets Model and Name from Chassis/CX_{nic_index}
    // Only applicable to Vera-Rubin. `nic_index` must be in 0..8.
    fn get_spx_nic_model_and_name<'a>(
        &'a self,
        nic_index: u8,
    ) -> RedfishFuture<'a, Result<Option<SpxNicModelAndName>, RedfishError>> {
        <standard::RedfishStandard as Redfish>::get_spx_nic_model_and_name(
            self.std_redfish(),
            nic_index,
        )
    }

    // Sets the NTP servers
    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> RedfishFuture<'a, Result<(), RedfishError>> {
        <standard::RedfishStandard as Redfish>::set_ntp_servers(self.std_redfish(), servers)
    }
}

/// Model and Name from a Vera-Rubin CX NIC chassis (`Chassis/CX_{index}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpxNicModelAndName {
    pub model: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Boot {
    Pxe,
    HardDisk,
    UefiHttp,
}

impl fmt::Display for Boot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// The current status of something (lockdown, serial_console), saying whether it has been enabled,
/// disabled, or the necessary settings are only partially applied.
#[derive(Clone, PartialEq, Debug)]
pub struct Status {
    pub(crate) status: StatusInternal,
    pub(crate) message: String,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
enum StatusInternal {
    Enabled,
    Partial,
    Disabled,
}

impl fmt::Display for StatusInternal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// BMC User Roles
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum RoleId {
    Administrator,
    Operator,
    ReadOnly,
    NoAccess,
}

impl fmt::Display for RoleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl Status {
    /// Did enabling complete successfully?
    pub fn is_fully_enabled(&self) -> bool {
        self.status == StatusInternal::Enabled
    }

    /// Did disabling complete successfuly (or thing was never enabled in the first place)?
    pub fn is_fully_disabled(&self) -> bool {
        self.status == StatusInternal::Disabled
    }

    /// Did lockdown enable/disable fail part way through, so we are partially locked?
    pub fn is_partially_enabled(&self) -> bool {
        self.status == StatusInternal::Partial
    }

    /// A vendor specific message detailing the individual status of the parts that are needed to
    /// enable or disabled. Format of message will change, do not parse.
    pub fn message(&self) -> &str {
        &self.message
    }

    // build_fake creates a Status for use in test environments, as its details are private.
    pub fn build_fake(enabled: EnabledDisabled) -> Self {
        Self {
            status: match enabled {
                EnabledDisabled::Enabled => StatusInternal::Enabled,
                EnabledDisabled::Disabled => StatusInternal::Disabled,
            },
            message: "Fake".to_string(),
        }
    }
}

/// How a caller identifies a boot interface to [`Redfish::machine_setup`]
/// and supporting query methods.
#[derive(Debug, Clone, Copy)]
pub enum BootInterfaceRef<'a> {
    /// MAC address of the boot interface. Vendor impl translates it into
    /// the vendor-native interface id its BIOS attributes consume.
    Mac(mac_address::MacAddress),
    /// Vendor-native Redfish `EthernetInterface.Id` for the boot
    /// interface (e.g. `"NIC.Slot.7-1-1"`). Vendor impl uses it
    /// directly.
    InterfaceId(&'a str),
    /// Complete identity for one boot interface. Both fields must identify the
    /// same interface; this is one target, not an instruction to try both.
    /// MAC-oriented vendor paths use `mac_address`, while interface-ID-oriented
    /// paths use `interface_id`.
    Pair {
        mac_address: mac_address::MacAddress,
        interface_id: &'a str,
    },
}

impl BootInterfaceRef<'_> {
    /// Returns the supplied MAC when this selector contains one.
    pub fn mac(&self) -> Option<mac_address::MacAddress> {
        match self {
            BootInterfaceRef::Mac(mac)
            | BootInterfaceRef::Pair {
                mac_address: mac, ..
            } => Some(*mac),
            BootInterfaceRef::InterfaceId(_) => None,
        }
    }
}

/// Returns the MAC address for a [`BootInterfaceRef`].
/// [`BootInterfaceRef::Mac`] and [`BootInterfaceRef::Pair`] pass through their
/// supplied MAC. [`BootInterfaceRef::InterfaceId`] is resolved by fetching
/// `Systems/{}/EthernetInterfaces/{id}` via the Redfish-standard
/// `EthernetInterface` resource (every vendor implements it).
///
/// Used by methods that compare against a MAC (verification paths that
/// walk boot options by MAC substring, etc.) so the caller can pass
/// either [`BootInterfaceRef`] variant uniformly.
pub async fn resolve_boot_interface_mac<R: Redfish + ?Sized>(
    redfish: &R,
    boot_interface: BootInterfaceRef<'_>,
) -> Result<String, RedfishError> {
    match boot_interface {
        BootInterfaceRef::Mac(mac)
        | BootInterfaceRef::Pair {
            mac_address: mac, ..
        } => Ok(mac.to_string()),
        BootInterfaceRef::InterfaceId(id) => {
            let eif = redfish.get_system_ethernet_interface(id).await?;
            extract_resolved_mac(eif.mac_address.as_deref(), id)
        }
    }
}

/// Pure half of [`resolve_boot_interface_mac`], split out for unit
/// tests. Returns the MAC if non-empty; errors otherwise rather than
/// passing an empty string through to downstream `.contains(&mac)`
/// matchers (which would match every option for an empty needle).
fn extract_resolved_mac(mac: Option<&str>, id: &str) -> Result<String, RedfishError> {
    let mac = mac.unwrap_or("");
    if mac.is_empty() {
        return Err(RedfishError::GenericError {
            error: format!("Systems/.../EthernetInterfaces/{id} has no populated MACAddress"),
        });
    }
    Ok(mac.to_string())
}

#[derive(Debug)]
pub struct MachineSetupStatus {
    pub is_done: bool,
    pub diffs: Vec<MachineSetupDiff>,
}

impl fmt::Display for MachineSetupStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_done {
            write!(f, "OK")
        } else {
            write!(
                f,
                "Mismatch: {:?}",
                self.diffs
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct MachineSetupDiff {
    pub key: String,
    pub expected: String,
    pub actual: String,
}

impl fmt::Display for MachineSetupDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is '{}' expected '{}'",
            self.key, self.actual, self.expected
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")] // No tag requried - this is not nested
pub enum JobState {
    Scheduled,
    ScheduledWithErrors,
    Running,
    Completed,
    CompletedWithErrors,
    Failed,
    Unknown,
}

impl JobState {
    /// Returns `true` when the job is in a terminal failure state and will not
    /// progress to completion.
    pub fn is_error_state(&self) -> bool {
        matches!(
            self,
            JobState::ScheduledWithErrors | JobState::CompletedWithErrors | JobState::Failed
        )
    }

    fn from_str(s: &str) -> JobState {
        match s {
            "Scheduled" => JobState::Scheduled,
            "Running" => JobState::Running,
            "Completed" => JobState::Completed,
            "CompletedWithErrors" => JobState::CompletedWithErrors,
            "Failed" => JobState::Failed,
            _ => JobState::Unknown,
        }
    }
}

#[derive(
    Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash, Copy, clap::ValueEnum, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum BiosProfileType {
    #[default]
    Performance,
    PowerEfficiency,
}

pub type BiosProfileProfiles = HashMap<BiosProfileType, HashMap<String, serde_json::Value>>;
pub type BiosProfileModel = HashMap<String, BiosProfileProfiles>;
pub type BiosProfileVendor = HashMap<RedfishVendor, BiosProfileModel>;

// Simplify model names so that we can put them in toml files as categories
pub fn model_coerce(original: &str) -> String {
    str::replace(original, " ", "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DefaultDelegatingRedfish {
        standard: standard::RedfishStandard,
    }

    impl Redfish for DefaultDelegatingRedfish {
        fn std_redfish(&self) -> &standard::RedfishStandard {
            &self.standard
        }
    }

    #[test]
    fn redfish_implementation_only_requires_standard_redfish() {
        fn assert_redfish<T: Redfish>() {}
        assert_redfish::<DefaultDelegatingRedfish>();
    }

    #[test]
    fn boot_interface_ref_mac_returns_inner() {
        let mac: mac_address::MacAddress = "AA:BB:CC:DD:EE:01".parse().unwrap();
        let r = BootInterfaceRef::Mac(mac);
        assert_eq!(r.mac(), Some(mac));
    }

    #[test]
    fn boot_interface_ref_interface_id_mac_is_none() {
        let r = BootInterfaceRef::InterfaceId("NIC.Slot.7-1-1");
        assert!(r.mac().is_none());
    }

    #[test]
    fn boot_interface_ref_pair_mac_returns_inner() {
        let mac: mac_address::MacAddress = "AA:BB:CC:DD:EE:01".parse().unwrap();
        let r = BootInterfaceRef::Pair {
            mac_address: mac,
            interface_id: "NIC.Slot.7-1-1",
        };
        assert_eq!(r.mac(), Some(mac));
    }

    #[tokio::test]
    async fn resolve_boot_interface_mac_uses_pair_mac_without_lookup() {
        let pool = RedfishClientPool::builder().build().unwrap();
        let redfish = pool
            .create_standard_client(Endpoint::default())
            .expect("test Redfish client should be constructed without a request");
        let mac: mac_address::MacAddress = "AA:BB:CC:DD:EE:01".parse().unwrap();

        let got = resolve_boot_interface_mac(
            redfish.as_ref(),
            BootInterfaceRef::Pair {
                mac_address: mac,
                interface_id: "NIC.Slot.7-1-1",
            },
        )
        .await
        .expect("pair should use its MAC without querying the empty endpoint");

        assert_eq!(got, mac.to_string());
    }

    #[test]
    fn extract_resolved_mac_passes_through_populated_mac() {
        let got = super::extract_resolved_mac(Some("AA:BB:CC:DD:EE:01"), "NIC.Slot.7-1-1")
            .expect("populated MAC should be returned as-is");
        assert_eq!(got, "AA:BB:CC:DD:EE:01");
    }

    #[test]
    fn extract_resolved_mac_errors_on_empty_string_mac() {
        // An empty MAC must error rather than pass through —
        // downstream `display_name.contains(&mac)` matchers would
        // otherwise match every boot option for an empty needle.
        let err = super::extract_resolved_mac(Some(""), "NIC.Slot.7-1-1")
            .expect_err("empty MAC should be an explicit error");
        let msg = err.to_string();
        assert!(
            msg.contains("NIC.Slot.7-1-1"),
            "error should name the interface id; got: {msg}",
        );
    }

    #[test]
    fn extract_resolved_mac_errors_on_missing_mac_field() {
        let err = super::extract_resolved_mac(None, "NIC.Slot.7-1-1")
            .expect_err("None MAC should be an explicit error");
        assert!(err.to_string().contains("NIC.Slot.7-1-1"));
    }
}
