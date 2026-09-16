use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DefaultOnNull};

use crate::model::BiosCommon;
use crate::model::InvalidValueError;
use crate::model::OnOff;
use crate::ODataId;
use crate::{model::ODataLinks, EnabledDisabled};

serde_with::with_prefix!(prefix_ssh "SSH.1.");
serde_with::with_prefix!(prefix_serial_redirection "SerialRedirection.1.");
serde_with::with_prefix!(prefix_current_nic "CurrentNIC.1.");
serde_with::with_prefix!(prefix_nic "NIC.1.");
serde_with::with_prefix!(prefix_current_ipv6 "CurrentIPv6.1.");
serde_with::with_prefix!(prefix_current_ipv4 "CurrentIPv4.1.");
serde_with::with_prefix!(prefix_ipv6 "IPv6.1.");
serde_with::with_prefix!(prefix_ipv4 "IPv4.1.");
serde_with::with_prefix!(prefix_logging "Logging.1.");
serde_with::with_prefix!(prefix_os_bmc "OS-BMC.1.");
serde_with::with_prefix!(prefix_info "Info.1.");
serde_with::with_prefix!(prefix_ipmi_lan "IPMILan.1.");
serde_with::with_prefix!(prefix_local_security "LocalSecurity.1.");
serde_with::with_prefix!(prefix_ipmi_sol "IPMISOL.1.");
serde_with::with_prefix!(prefix_platform_capability "PlatformCapability.1.");
serde_with::with_prefix!(prefix_racadm "Racadm.1.");
serde_with::with_prefix!(prefix_redfish_eventing "RedfishEventing.1.");
serde_with::with_prefix!(prefix_rfs "RFS.1.");
serde_with::with_prefix!(prefix_security "Security.1.");
serde_with::with_prefix!(prefix_security_certificate1 "SecurityCertificate.1.");
serde_with::with_prefix!(prefix_security_certificate2 "SecurityCertificate.2.");
serde_with::with_prefix!(prefix_serial "Serial.1.");
serde_with::with_prefix!(prefix_service_module "ServiceModule.1.");
serde_with::with_prefix!(prefix_server_boot "ServerBoot.1.");
serde_with::with_prefix!(prefix_support_assist "SupportAssist.1.");
serde_with::with_prefix!(prefix_sys_info "SysInfo.1.");
serde_with::with_prefix!(prefix_sys_log "SysLog.1.");
serde_with::with_prefix!(prefix_time "Time.1.");
serde_with::with_prefix!(prefix_virtual_console "VirtualConsole.1.");
serde_with::with_prefix!(prefix_virtual_media "VirtualMedia.1.");
serde_with::with_prefix!(prefix_vnc_server "VNCServer.1.");
serde_with::with_prefix!(prefix_web_server "WebServer.1.");
serde_with::with_prefix!(prefix_update "Update.1.");

serde_with::with_prefix!(prefix_users1 "Users.1.");
serde_with::with_prefix!(prefix_users2 "Users.2.");
serde_with::with_prefix!(prefix_users3 "Users.3.");
serde_with::with_prefix!(prefix_users4 "Users.4.");
serde_with::with_prefix!(prefix_users5 "Users.5.");
serde_with::with_prefix!(prefix_users6 "Users.6.");
serde_with::with_prefix!(prefix_users7 "Users.7.");
serde_with::with_prefix!(prefix_users8 "Users.8.");
serde_with::with_prefix!(prefix_users9 "Users.9.");
serde_with::with_prefix!(prefix_users10 "Users.10.");
serde_with::with_prefix!(prefix_users11 "Users.11.");
serde_with::with_prefix!(prefix_users12 "Users.12.");
serde_with::with_prefix!(prefix_users13 "Users.13.");
serde_with::with_prefix!(prefix_users14 "Users.14.");
serde_with::with_prefix!(prefix_users15 "Users.15.");
serde_with::with_prefix!(prefix_users16 "Users.16.");

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct IDracCard {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub description: String,
    #[serde(rename = "IPMIVersion")]
    pub ipmi_version: String,
    pub id: String,
    pub last_system_inventory_time: String,
    pub last_update_time: String,
    pub name: String,
    #[serde(rename = "URLString")]
    pub url_string: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Manager {
    #[serde(rename = "DelliDRACCard")]
    pub dell_idrac_card: IDracCard,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SystemWrapper {
    pub dell_system: System,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct System {
    #[serde(rename = "BIOSReleaseDate")]
    pub bios_release_date: String,
    pub chassis_service_tag: String,
    pub chassis_system_height_unit: i64,
    pub estimated_exhaust_temperature_celsius: Option<i64>,
    #[serde(rename = "EstimatedSystemAirflowCFM")]
    pub estimated_system_airflow_cfm: Option<i64>,
    pub express_service_code: String,
    pub fan_rollup_status: Option<String>, // null->None if machine is off
    pub intrusion_rollup_status: String,
    pub managed_system_size: String,
    #[serde(rename = "MaxCPUSockets")]
    pub max_cpu_sockets: i64,
    #[serde(rename = "MaxDIMMSlots")]
    pub max_dimm_slots: i64,
    #[serde(rename = "MaxPCIeSlots")]
    pub max_pcie_slots: i64,
    #[serde(rename = "PopulatedDIMMSlots")]
    pub populated_dimm_slots: i64,
    #[serde(rename = "PopulatedPCIeSlots")]
    pub populated_pcie_slots: i64,
    pub power_cap_enabled_state: Option<String>, // We see this field explicitly returned as null by Dell XE9680s
    pub system_generation: String,
    #[serde(default)]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub temp_rollup_status: String, // Observed as null if machine is off, kept as String for backwards-compat
    #[serde(rename = "UUID")]
    pub uuid: String,
    pub volt_rollup_status: String,
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, Eq, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub enum BootDevices {
    Normal,
    PXE,
    HDD,
    BIOS,
    FDD,
    SD,
    F10,
    F11,
    UefiHttp,
}

impl fmt::Display for BootDevices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ServerBoot {
    pub boot_once: EnabledDisabled,
    pub first_boot_device: BootDevices,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ServerBootAttrs {
    #[serde(flatten, with = "prefix_server_boot")]
    pub server_boot: ServerBoot,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetFirstBootDevice {
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: ServerBootAttrs,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "PascalCase")]
pub struct SetSettingsApplyTime {
    pub apply_time: RedfishSettingsApplyTime,
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub enum RedfishSettingsApplyTime {
    AtMaintenanceWindowStart,
    Immediate, // for idrac settings
    InMaintenanceWindowOnReset,
    OnReset, // for bios settings
}

impl fmt::Display for RedfishSettingsApplyTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BiosLockdownAttrs {
    pub in_band_manageability_interface: EnabledDisabled,
    pub uefi_variable_access: UefiVariableAccessSettings,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetBiosLockdownAttrs {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: BiosLockdownAttrs,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum UefiVariableAccessSettings {
    Standard,
    Controlled,
}

impl fmt::Display for UefiVariableAccessSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BmcLockdown {
    #[serde(
        rename = "Lockdown.1.SystemLockdown",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_lockdown: Option<EnabledDisabled>,
    #[serde(rename = "Racadm.1.Enable", skip_serializing_if = "Option::is_none")]
    pub racadm_enable: Option<EnabledDisabled>,
    #[serde(
        flatten,
        with = "prefix_server_boot",
        skip_serializing_if = "Option::is_none"
    )]
    pub server_boot: Option<ServerBoot>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetBmcLockdown {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: BmcLockdown,
}

// aggregate all required bios settings in one struct for one shot
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct MachineBiosAttrs {
    pub in_band_manageability_interface: EnabledDisabled,
    pub uefi_variable_access: UefiVariableAccessSettings,
    pub serial_comm: SerialCommSettings,
    pub serial_port_address: SerialPortSettings,
    pub fail_safe_baud: String,
    pub con_term_type: SerialPortTermSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redir_after_boot: Option<EnabledDisabled>,
    pub sriov_global_enable: EnabledDisabled,
    pub tpm_security: OnOff,
    pub tpm2_hierarchy: Tpm2HierarchySettings,
    pub tpm2_algorithm: Tpm2Algorithm,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_mode: Option<String>,
    #[serde(rename = "HttpDev1EnDis")]
    pub http_device_1_enabled_disabled: EnabledDisabled,
    #[serde(rename = "PxeDev1EnDis")]
    pub pxe_device_1_enabled_disabled: EnabledDisabled,
    #[serde(rename = "HttpDev1Interface")]
    pub http_device_1_interface: String,
    pub set_boot_order_en: String,
    #[serde(rename = "HttpDev1TlsMode")]
    pub http_device_1_tls_mode: TlsMode,
    pub set_boot_order_dis: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetBiosAttrs {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: MachineBiosAttrs,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct GenericSetBiosAttrs {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BiosSerialAttrs {
    pub serial_comm: SerialCommSettings,
    pub serial_port_address: SerialPortSettings,
    pub ext_serial_connector: SerialPortExtSettings,
    pub fail_safe_baud: String,
    pub con_term_type: SerialPortTermSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redir_after_boot: Option<EnabledDisabled>, // Not available in iDRAC 10
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetBiosSerialAttrs {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: BiosSerialAttrs,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum SerialCommSettings {
    OnConRedir, // iDRAC 9 - preferred
    OnNoConRedir,
    OnConRedirAuto, // newer iDRAC - preferred
    OnConRedirCom1, // newer iDRAC
    OnConRedirCom2, // newer iDRAC
    Off,
}

impl fmt::Display for SerialCommSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl FromStr for SerialCommSettings {
    type Err = InvalidValueError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "OnConRedir" => Ok(Self::OnConRedir),
            "OnNoConRedir" => Ok(Self::OnNoConRedir),
            "OnConRedirAuto" => Ok(Self::OnConRedirAuto),
            "OnConRedirCom1" => Ok(Self::OnConRedirCom1),
            "OnConRedirCom2" => Ok(Self::OnConRedirCom2),
            "Off" => Ok(Self::Off),
            x => Err(InvalidValueError(format!(
                "Invalid SerialCommSettings value: {x}"
            ))),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum SerialPortSettings {
    Com1,                   // legacy preferred
    Com2,                   // legacy
    Serial1Com1Serial2Com2, // newer BIOS: SD1=COM1, SD2=COM2
    Serial1Com2Serial2Com1, // newer BIOS: SD1=COM2, SD2=COM1 (preferred for SOL)
}

impl fmt::Display for SerialPortSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum SerialPortExtSettings {
    Serial1, // preferred
    Serial2,
    RemoteAccDevice,
}

impl fmt::Display for SerialPortExtSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum SerialPortTermSettings {
    Vt100Vt220, // preferred
    Ansi,
}

impl fmt::Display for SerialPortTermSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetBmcRemoteAccess {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: BmcRemoteAccess,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BmcRemoteAccess {
    #[serde(rename = "SSH.1.Enable")]
    pub ssh_enable: EnabledDisabled,
    #[serde(flatten, with = "prefix_serial_redirection")]
    pub serial_redirection: SerialRedirection,
    #[serde(rename = "IPMILan.1.Enable")]
    pub ipmi_lan_enable: EnabledDisabled,
    #[serde(flatten, with = "prefix_ipmi_sol")]
    pub ipmi_sol: IpmiSol,
    // in future add virtualconsole, virtualmedia, vncserver if needed
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct IpmiSol {
    pub baud_rate: String, //SerialBaudRates,
    pub enable: EnabledDisabled,
    pub min_privilege: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SerialRedirection {
    pub enable: EnabledDisabled, // ensure this is enabled
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BiosTpmAttrs {
    pub tpm_security: OnOff,
    pub tpm2_hierarchy: Tpm2HierarchySettings,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SetBiosTpmAttrs {
    #[serde(rename = "@Redfish.SettingsApplyTime")]
    pub redfish_settings_apply_time: SetSettingsApplyTime,
    pub attributes: BiosTpmAttrs,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum Tpm2HierarchySettings {
    Enabled,
    Disabled,
    Clear,
}

impl fmt::Display for Tpm2HierarchySettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum Tpm2Algorithm {
    SHA1,
    SHA128,
    SHA256,
    SHA384,
    SHA512,
    SM3,
}

impl fmt::Display for Tpm2Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub enum TlsMode {
    None,
    OneWay,
}

impl fmt::Display for TlsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Bios {
    #[serde(flatten)]
    pub common: BiosCommon,
    #[serde(rename = "@odata.context")]
    pub odata_context: String,
    pub attributes: BiosAttributes,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
// The Option fields are present on PowerEdge R750 but not on PowerEdge R640
pub struct BiosAttributes {
    pub system_model_name: Option<String>,
    pub system_bios_version: Option<String>,
    pub system_me_version: Option<String>,
    pub system_service_tag: Option<String>,
    pub system_manufacturer: Option<String>,
    pub sys_mfr_contact_info: Option<String>,
    pub system_cpld_version: Option<String>,
    pub uefi_compliance_version: Option<String>,
    pub proc_core_speed: Option<String>,
    pub proc_bus_speed: Option<String>,
    pub proc_1_id: Option<String>,
    pub proc_1_brand: Option<String>,
    pub proc_1_l2_cache: Option<String>,
    pub proc_1_l3_cache: Option<String>,
    pub proc_1_max_memory_capacity: Option<String>,
    pub proc_1_microcode: Option<String>,
    pub proc_2_id: Option<String>,
    pub proc_2_brand: Option<String>,
    pub proc_2_l2_cache: Option<String>,
    pub proc_2_l3_cache: Option<String>,
    pub proc_2_max_memory_capacity: Option<String>,
    pub proc_2_microcode: Option<String>,
    pub current_emb_video_state: Option<String>,
    pub aes_ni: Option<String>,
    pub tpm_info: Option<String>,
    pub tpm_firmware: Option<String>,
    pub sys_mem_size: Option<String>,
    pub sys_mem_type: Option<String>,
    pub sys_mem_speed: Option<String>,
    pub sys_mem_volt: Option<String>,
    pub video_mem: Option<String>,
    pub asset_tag: Option<String>,
    #[serde(rename = "SHA256SystemPassword")]
    pub sha256_system_password: Option<String>,
    #[serde(rename = "SHA256SystemPasswordSalt")]
    pub sha256_system_password_salt: Option<String>,
    #[serde(rename = "SHA256SetupPassword")]
    pub sha256_setup_password: Option<String>,
    #[serde(rename = "SHA256SetupPasswordSalt")]
    pub sha256_setup_password_salt: Option<String>,
    pub proc1_num_cores: Option<i64>,
    pub proc2_num_cores: Option<i64>,
    pub controlled_turbo_minus_bin: Option<i64>,
    pub logical_proc: Option<String>,
    pub cpu_interconnect_bus_speed: Option<String>,
    pub proc_virtualization: Option<String>,
    pub kernel_dma_protection: Option<String>,
    pub directory_mode: Option<String>,
    pub proc_adj_cache_line: Option<String>,
    pub proc_hw_prefetcher: Option<String>,
    pub dcu_streamer_prefetcher: Option<String>,
    pub dcu_ip_prefetcher: Option<String>,
    pub sub_numa_cluster: Option<String>,
    pub madt_core_enumeration: Option<String>,
    pub upi_prefetch: Option<String>,
    pub xpt_prefetch: Option<String>,
    pub llc_prefetch: Option<String>,
    pub dead_line_llc_alloc: Option<String>,
    pub dynamic_core_allocation: Option<String>,
    pub proc_avx_p1: Option<String>,
    pub processor_active_pbf: Option<String>,
    pub processor_rapl_prioritization: Option<String>,
    pub proc_x2_apic: Option<String>,
    pub avx_iccp_pre_grant_license: Option<String>,
    pub proc_cores: Option<String>,
    pub lmce_en: Option<String>,
    pub controlled_turbo: Option<String>,
    pub optimizer_mode: Option<String>,
    pub emb_sata: Option<String>,
    pub security_freeze_lock: Option<String>,
    pub write_cache: Option<String>,
    pub nvme_mode: Option<String>,
    pub bios_nvme_driver: Option<String>,
    pub boot_mode: Option<String>,
    pub boot_seq_retry: Option<String>,
    pub hdd_failover: Option<String>,
    pub generic_usb_boot: Option<String>,
    pub hdd_placeholder: Option<String>,
    pub sys_prep_clean: Option<String>,
    pub one_time_boot_mode: Option<String>,
    pub one_time_uefi_boot_seq_dev: Option<String>,
    pub pxe_dev1_en_dis: Option<String>,
    pub pxe_dev2_en_dis: Option<String>,
    pub pxe_dev3_en_dis: Option<String>,
    pub pxe_dev4_en_dis: Option<String>,
    pub pxe_dev1_interface: Option<String>,
    pub pxe_dev1_protocol: Option<String>,
    pub pxe_dev1_vlan_en_dis: Option<String>,
    pub pxe_dev2_interface: Option<String>,
    pub pxe_dev2_protocol: Option<String>,
    pub pxe_dev2_vlan_en_dis: Option<String>,
    pub pxe_dev3_interface: Option<String>,
    pub pxe_dev3_protocol: Option<String>,
    pub pxe_dev3_vlan_en_dis: Option<String>,
    pub pxe_dev4_interface: Option<String>,
    pub pxe_dev4_protocol: Option<String>,
    pub pxe_dev4_vlan_en_dis: Option<String>,
    pub usb_ports: Option<String>,
    pub usb_managed_port: Option<String>,
    pub emb_nic1_nic2: Option<String>,
    pub ioat_engine: Option<String>,
    pub emb_video: Option<String>,
    pub snoop_hld_off: Option<String>,
    pub sriov_global_enable: Option<String>,
    pub os_watchdog_timer: Option<String>,
    #[serde(rename = "PCIRootDeviceUnhide")]
    pub pci_root_device_unhide: Option<String>,
    pub mmio_above4_gb: Option<String>,
    #[serde(rename = "MemoryMappedIOH")]
    pub memory_mapped_ioh: Option<String>,
    pub dell_auto_discovery: Option<String>,
    pub serial_comm: Option<String>,
    pub serial_port_address: Option<String>,
    pub ext_serial_connector: Option<String>,
    pub fail_safe_baud: Option<String>,
    pub con_term_type: Option<String>,
    pub redir_after_boot: Option<String>,
    pub sys_profile: Option<String>,
    pub proc_pwr_perf: Option<String>,
    pub mem_frequency: Option<String>,
    pub proc_turbo_mode: Option<String>,
    #[serde(rename = "ProcC1E")]
    pub proc_c1e: Option<String>,
    #[serde(rename = "ProcCStates")]
    pub proc_cstates: Option<String>,
    pub mem_patrol_scrub: Option<String>,
    pub mem_refresh_rate: Option<String>,
    pub uncore_frequency: Option<String>,
    pub energy_performance_bias: Option<String>,
    pub monitor_mwait: Option<String>,
    pub workload_profile: Option<String>,
    pub cpu_interconnect_bus_link_power: Option<String>,
    pub pcie_aspm_l1: Option<String>,
    pub password_status: Option<String>,
    pub tpm_security: Option<String>,
    pub tpm2_hierarchy: Option<String>,
    pub intel_txt: Option<String>,
    pub memory_encryption: Option<String>,
    pub intel_sgx: Option<String>,
    pub pwr_button: Option<String>,
    pub ac_pwr_rcvry: Option<String>,
    pub ac_pwr_rcvry_delay: Option<String>,
    pub uefi_variable_access: Option<String>,
    pub in_band_manageability_interface: Option<String>,
    pub smm_security_mitigation: Option<String>,
    pub secure_boot: Option<String>,
    pub secure_boot_policy: Option<String>,
    pub secure_boot_mode: Option<String>,
    pub authorize_device_firmware: Option<String>,
    pub tpm_ppi_bypass_provision: Option<String>,
    pub tpm_ppi_bypass_clear: Option<String>,
    pub tpm2_algorithm: Option<String>,
    pub redundant_os_location: Option<String>,
    pub redundant_os_state: Option<String>,
    pub redundant_os_boot: Option<String>,
    pub mem_test: Option<String>,
    pub mem_op_mode: Option<String>,
    #[serde(rename = "FRMPercent")]
    pub frm_percent: Option<String>,
    pub node_interleave: Option<String>,
    pub memory_training: Option<String>,
    pub corr_ecc_smi: Option<String>,
    #[serde(rename = "CECriticalSEL")]
    pub ce_critical_sel: Option<String>,
    #[serde(rename = "PPROnUCE")]
    pub ppr_on_uce: Option<String>,
    pub num_lock: Option<String>,
    pub err_prompt: Option<String>,
    pub force_int10: Option<String>,
    #[serde(rename = "DellWyseP25BIOSAccess")]
    pub dell_wyse_p25_bios_access: Option<String>,
    pub power_cycle_request: Option<String>,
    pub sys_password: Option<String>,
    pub setup_password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SupportAssist {
    pub default_protocol_port: i64,
    #[serde(rename = "HostOSProxyAddress")]
    pub host_os_proxy_address: String,
    #[serde(rename = "HostOSProxyUserName")]
    pub host_os_proxy_user_name: String,
    #[serde(rename = "HostOSProxyPassword")]
    pub host_os_proxy_password: Option<String>,
    #[serde(rename = "HostOSProxyPort")]
    pub host_os_proxy_port: i64,
    pub default_protocol: String,
    pub email_opt_in: String,
    pub event_based_auto_collection: String,
    pub filter_auto_collections: String,
    #[serde(rename = "HostOSProxyConfigured")]
    pub host_os_proxy_configured: String,
    #[serde(rename = "NativeOSLogsCollectionSupported")]
    pub native_os_logs_collection_supported: String,
    pub preferred_language: String,
    pub pro_support_plus_recommendations_report: String,
    pub request_technician_for_parts_dispatch: String,
    pub support_assist_enable_state: String, // ensure this is disabled
    #[serde(rename = "DefaultIPAddress")]
    pub default_ip_address: String,
    pub default_share_name: String,
    pub default_user_name: String,
    pub default_password: Option<String>,
    pub default_workgroup_name: String,
    #[serde(rename = "RegistrationID")]
    pub registration_id: String,
    #[serde(rename = "iDRACFirstPowerUpDateTime")]
    pub idrac_first_power_up_date_time: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BmcNic {
    #[serde(rename = "DedicatedNICScanTime")]
    pub dedicated_nic_scan_time: i64,
    #[serde(rename = "MTU")]
    pub mtu: i64, // ensure this is correct
    #[serde(rename = "NumberOfLOM")]
    pub number_of_lom: Option<i64>,
    #[serde(rename = "SharedNICScanTime")]
    pub shared_nic_scan_time: i64,
    #[serde(rename = "VLanID")]
    pub vlan_id: i64,
    #[serde(rename = "VLanPriority")]
    pub vlan_priority: i64,
    #[serde(rename = "ActiveNIC")]
    pub active_nic: Option<String>,
    #[serde(rename = "ActiveSharedLOM")]
    pub active_shared_lom: Option<String>,
    pub auto_config: Option<String>,
    pub auto_detect: String,
    pub autoneg: String, // ensure this is enabled
    #[serde(rename = "DNSDomainFromDHCP")]
    pub dns_domain_from_dhcp: String,
    #[serde(rename = "DNSDomainNameFromDHCP")]
    pub dns_domain_name_from_dhcp: Option<String>,
    #[serde(rename = "DNSRegister")]
    pub dns_register: String,
    #[serde(rename = "DNSRegisterInterval")]
    pub dns_register_interval: Option<i64>,
    #[serde(rename = "DiscoveryLLDP")]
    pub discovery_lldp: Option<String>,
    pub duplex: String,
    pub enable: String, // ensure this is enabled
    pub failover: String,
    pub link_status: Option<String>,
    pub ping_enable: String,
    pub selection: Option<String>,
    pub speed: String,
    pub topology_lldp: Option<String>,
    #[serde(rename = "VLanEnable")]
    pub vlan_enable: String,
    #[serde(rename = "VLanPort")]
    pub vlan_port: Option<String>,
    #[serde(rename = "VLanSetting")]
    pub vlan_setting: Option<String>,
    #[serde(rename = "DNSDomainName")]
    pub dns_domain_name: String,
    #[serde(rename = "DNSRacName")]
    pub dns_rac_name: String,
    #[serde(rename = "MACAddress")]
    pub mac_address: String,
    #[serde(rename = "MACAddress2")]
    pub mac_address2: Option<String>,
    pub mgmt_iface_name: Option<String>,
    pub switch_connection: Option<String>,
    pub switch_port_connection: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SysInfo {
    pub local_console_lock_out: i64,
    #[serde(rename = "POSTCode")]
    pub post_code: i64,
    pub system_rev: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BmcIpv6 {
    #[serde(rename = "IPV6NumOfExtAddress")]
    pub num_of_ext_address: Option<i64>,
    pub prefix_length: i64,
    pub address1: String,
    pub address2: String,
    pub address3: String,
    pub address4: String,
    pub address5: String,
    pub address6: String,
    pub address7: String,
    pub address8: String,
    pub address9: String,
    pub address10: String,
    pub address11: String,
    pub address12: String,
    pub address13: String,
    pub address14: String,
    pub address15: String,
    #[serde(rename = "DHCPv6Address")]
    pub dhcpv6_address: Option<String>,
    #[serde(rename = "DNS1")]
    pub dns1: String,
    #[serde(rename = "DNS2")]
    pub dns2: String,
    #[serde(rename = "DUID")]
    pub duid: String,
    pub gateway: String,
    pub link_local_address: String,
    pub address_generation_mode: String,
    pub address_state: Option<String>,
    pub auto_config: String,
    #[serde(rename = "DNSFromDHCP6")]
    pub dns_from_dhcp6: String,
    pub enable: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BmcIpv4 {
    #[serde(rename = "DHCPEnable")]
    pub dhcp_enable: String,
    #[serde(rename = "DNSFromDHCP")]
    pub dns_from_dhcp: String,
    pub enable: String,
    pub address: String,
    pub netmask: String,
    pub gateway: String,
    #[serde(rename = "DNS1")]
    pub dns1: String,
    #[serde(rename = "DNS2")]
    pub dns2: String,
    pub dup_addr_detected: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Users {
    pub privilege: i64,
    pub authentication_protocol: String,
    pub enable: String,
    pub ipmi_lan_privilege: String,
    pub ipmi_serial_privilege: String,
    pub privacy_protocol: String,
    pub protocol_enable: String,
    #[serde(rename = "Simple2FA")]
    pub simple_2fa: String,
    pub sol_enable: String,
    pub use_email: String,
    #[serde(rename = "UseSMS")]
    pub use_sms: String,
    pub email_address: String,
    #[serde(rename = "IPMIKey")]
    pub ipmi_key: String,
    #[serde(rename = "MD5v3Key")]
    pub md5_v3_key: String,
    #[serde(rename = "SHA1v3Key")]
    pub sha1_v3_key: String,
    #[serde(rename = "SHA256Password")]
    pub sha256_password: String,
    #[serde(rename = "SHA256PasswordSalt")]
    pub sha256_password_salt: String,
    #[serde(rename = "SMSNumber")]
    pub sms_number: String,
    pub user_name: String,
    pub password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SysLog {
    pub port: i64,
    pub power_log_interval: i64,
    pub power_log_enable: String,
    pub sys_log_enable: String, // ensure this is disabled
    pub server1: String,
    pub server2: String,
    pub server3: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct RedfishEventing {
    pub delivery_retry_attempts: i64,
    pub delivery_retry_interval_in_seconds: i64,
    pub ignore_certificate_errors: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Time {
    pub day_light_offset: i64,
    pub time_zone_offset: i64,
    pub timezone: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Ssh {
    pub max_sessions: i64,
    pub port: i64,
    pub timeout: i64,
    pub enable: String,
    pub banner: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Security {
    pub password_minimum_length: i64,
    #[serde(rename = "FIPSMode")]
    pub fips_mode: String,
    pub minimum_password_score: String,
    pub password_require_numbers: String,
    pub password_require_symbols: String,
    pub password_require_upper_case: String,
    pub password_require_regex: String,
    pub csr_common_name: String,
    pub csr_country_code: String,
    pub csr_email_addr: String,
    pub csr_locality_name: String,
    pub csr_organization_name: String,
    pub csr_organization_unit: String,
    pub csr_state_name: String,
    pub csr_subject_alt_name: String,
    pub csr_key_size: String,
    #[serde(rename = "FIPSVersion")]
    pub fips_version: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct WebServer {
    pub http_port: i64,
    pub https_port: i64,
    pub max_number_of_sessions: i64,
    pub timeout: i64,
    #[serde(rename = "BlockHTTPPort")]
    pub block_http_port: String,
    pub enable: String, // ensure this is enabled
    pub host_header_check: String,
    pub http2_enable: String,
    pub https_redirection: String, // ensure this is enabled
    pub lower_encryption_bit_length: String,
    #[serde(rename = "SSLEncryptionBitLength")]
    pub ssl_encryption_bit_length: String,
    #[serde(rename = "TLSProtocol")]
    pub tls_protocol: String,
    pub title_bar_option: String,
    pub title_bar_option_custom: String,
    pub custom_cipher_string: String,
    #[serde(rename = "ManualDNSEntry")]
    pub manual_dns_entry: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SecurityCertificate {
    pub cert_valid_from: String,
    pub cert_valid_to: String,
    pub issuer_common_name: String,
    pub issuer_country_code: String,
    pub issuer_locality: String,
    pub issuer_organization: String,
    pub issuer_organizational_unit: String,
    pub issuer_state: String,
    pub serial_number: String, // not an identifier
    pub subject_common_name: String,
    pub subject_country_code: String,
    pub subject_locality: String,
    pub subject_organization: String,
    pub subject_organizational_unit: String,
    pub subject_state: String,
    pub certificate_instance: i64,
    pub certificate_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PlatformCapability {
    #[serde(rename = "ASHRAECapable")]
    pub ashrae_capable: String,
    pub backup_restore_capable: String,
    #[serde(rename = "CUPSCapable")]
    pub cups_capable: String,
    pub front_panel_capable: String,
    #[serde(rename = "FrontPanelUSBCapable")]
    pub front_panel_usb_capable: String,
    #[serde(rename = "FrontPortUSBConfiguration")]
    pub front_port_usb_configuration: String,
    pub grid_current_cap_capable: String,
    #[serde(rename = "LCDCapable")]
    pub lcd_capable: String,
    pub live_scan_capable: String,
    #[serde(rename = "NicVLANCapable")]
    pub nic_vlan_capable: String,
    #[serde(rename = "PMBUSCapablePSU")]
    pub pmbus_capable_psu: String,
    pub power_budget_capable: String,
    pub power_monitoring_capable: String,
    #[serde(rename = "SerialDB9PCapable")]
    pub serial_db9p_capable: String,
    pub server_allocation_capable: String,
    pub system_current_cap_capable: String,
    pub user_power_cap_bound_capable: String,
    pub user_power_cap_capable: String,
    pub wi_fi_capable: String,
    #[serde(rename = "vFlashCapable")]
    pub vflash_capable: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ServiceModule {
    #[serde(rename = "ChipsetSATASupported")]
    pub chipset_sata_supported: String,
    #[serde(rename = "HostSNMPAlert")]
    pub host_snmp_alert: String,
    #[serde(rename = "HostSNMPGet")]
    pub host_snmp_get: String,
    #[serde(rename = "HostSNMPOMSAAlert")]
    pub host_snmp_omsa_alert: String,
    #[serde(rename = "LCLReplication")]
    pub lcl_replication: String,
    #[serde(rename = "OMSAPresence")]
    pub omsa_presence: String,
    #[serde(rename = "OSInfo")]
    pub os_info: String,
    #[serde(rename = "SSEventCorrelation")]
    pub ss_event_correlation: String,
    pub service_module_enable: String,
    pub service_module_state: String,
    #[serde(rename = "WMIInfo")]
    pub wmi_info: String,
    pub watchdog_recovery_action: String,
    pub watchdog_state: String,
    #[serde(rename = "iDRACHardReset")]
    pub idrac_hard_reset: String,
    #[serde(rename = "iDRACSSOLauncher")]
    pub idrac_sso_launcher: String,
    pub service_module_version: String,
    pub watchdog_reset_time: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct VirtualConsole {
    pub active_sessions: i64,
    pub max_sessions: i64,
    pub port: i64,
    pub timeout: i64,
    pub access_privilege: String,
    pub attach_state: String,
    pub close_unused_port: String,
    pub enable: String,
    pub encrypt_enable: String,
    pub local_disable: String,
    pub local_video: String,
    pub plugin_type: String,
    pub timeout_enable: String,
    pub web_redirect: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct VirtualMedia {
    pub active_sessions: i64,
    pub max_sessions: i64,
    pub attached: String,
    pub boot_once: String,
    pub enable: String, // ensure this is disabled
    pub encrypt_enable: String,
    pub floppy_emulation: String,
    pub key_enable: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Racadm {
    pub max_sessions: i64,
    pub timeout: i64,
    pub enable: String, // ensure this is disabled
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Info {
    pub server_gen: String,
    #[serde(rename = "Type")]
    pub server_type: String,
    pub build: String,
    #[serde(rename = "CPLDVersion")]
    pub cpld_version: String, // audit, ensure this is >= min required
    pub description: String,
    #[serde(rename = "HWRev")]
    pub hw_rev: String,
    #[serde(rename = "IPMIVersion")]
    pub ipmi_version: String,
    pub name: String,
    pub product: String,
    pub rollback_build: String,
    pub rollback_version: String,
    pub version: String, // audit, ensure this is >= min required
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct IpmiLan {
    pub alert_enable: String,
    pub enable: String,
    pub priv_limit: String,
    pub community_name: String,
    pub encryption_key: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct VncServer {
    pub active_sessions: i64,
    pub max_sessions: i64,
    pub port: i64,
    pub timeout: i64,
    pub enable: String, // ensure this is disabled
    pub lower_encryption_bit_length: String,
    #[serde(rename = "SSLEncryptionBitLength")]
    pub ssl_encryption_bit_length: String,
    pub password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct OsBmc {
    pub admin_state: String,
    #[serde(rename = "PTCapability")]
    pub pt_capability: String,
    #[serde(rename = "PTMode")]
    pub pt_mode: String,
    pub usb_nic_ipv4_address_support: String,
    pub os_ip_address: String,
    pub usb_nic_ip_address: String,
    pub usb_nic_ip_v6_address: String,
    #[serde(rename = "UsbNicULA")]
    pub usb_nic_ula: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Rfs {
    pub attach_mode: String,
    pub enable: String, // ensure this is disabled
    pub ignore_cert_warning: String,
    pub media_attach_state: String,
    pub status: String,
    pub write_protected: String,
    pub image: String,
    pub user: String,
    pub password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Serial {
    // this is the idrac serial config, not for the x86
    pub history_size: i64,
    pub idle_timeout: i64,
    pub baud_rate: String, //SerialBaudRates,
    pub enable: String,
    pub flow_control: String,
    pub no_auth: String,
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct LocalSecurity {
    pub local_config: String,
    pub preboot_config: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Logging {
    #[serde(rename = "SELBufferType")]
    pub sel_buffer_type: String,
    #[serde(rename = "SELOEMEventFilterEnable")]
    pub sel_oem_event_filter_enable: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Update {
    #[serde(rename = "FwUpdateTFTPEnable")]
    pub fw_update_tftp_enable: String,
    #[serde(rename = "FwUpdateIPAddr")]
    pub fw_update_ip_addr: String,
    pub fw_update_path: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Attributes {
    #[serde(rename = "Lockdown.1.SystemLockdown")]
    pub system_lockdown: String, // ensure this is set
    #[serde(rename = "Redfish.1.Enable")]
    pub redfish_enable: String,

    #[serde(flatten, with = "prefix_ssh")]
    pub ssh: Ssh, // ensure this is configured
    #[serde(flatten, with = "prefix_serial_redirection")]
    pub serial_redirection: SerialRedirection, // ensure this is configured

    #[serde(rename = "PCIeVDM.1.Enable")]
    pub pcie_vdm_enable: String,
    #[serde(rename = "IntegratedDatacenter.1.DiscoveryEnable")]
    pub integrated_datacenter_discovery_enable: String,
    #[serde(rename = "ASRConfig.1.Enable")]
    pub asr_config_enable: String,
    #[serde(rename = "SwitchConnectionView.1.Enable")]
    pub switch_connection_view_enable: String,
    #[serde(rename = "SecureDefaultPassword.1.ForceChangePassword")]
    pub force_change_password: String,
    #[serde(rename = "DefaultCredentialMitigationConfigGroup.1.DefaultCredentialMitigation")]
    pub default_credential_mitigation: String,
    #[serde(rename = "AutoOSLockGroup.1.AutoOSLockState")]
    pub auto_os_lock_state: String,

    #[serde(flatten, with = "prefix_nic")]
    pub nic: BmcNic,
    #[serde(flatten, with = "prefix_ipv4")]
    pub ipv4: BmcIpv4,
    #[serde(flatten, with = "prefix_ipv6")]
    pub ipv6: BmcIpv6,

    #[serde(flatten, with = "prefix_current_nic")]
    pub current_nic: BmcNic,
    #[serde(flatten, with = "prefix_current_ipv4")]
    pub current_ipv4: BmcIpv4,
    #[serde(flatten, with = "prefix_current_ipv6")]
    pub current_ipv6: BmcIpv6,

    #[serde(flatten, with = "prefix_info")]
    pub info: Info,
    #[serde(flatten, with = "prefix_ipmi_lan")]
    pub ipmi_lan: IpmiLan,
    #[serde(flatten, with = "prefix_local_security")]
    pub local_security: LocalSecurity,
    #[serde(flatten, with = "prefix_logging")]
    pub logging: Logging,
    #[serde(flatten, with = "prefix_os_bmc")]
    pub os_bmc: OsBmc,
    #[serde(flatten, with = "prefix_platform_capability")]
    pub platform_capability: PlatformCapability,
    #[serde(flatten, with = "prefix_racadm")]
    pub racadm: Racadm,
    #[serde(flatten, with = "prefix_redfish_eventing")]
    pub redfish_eventing: RedfishEventing,
    #[serde(flatten, with = "prefix_rfs")]
    pub rfs: Rfs,
    #[serde(flatten, with = "prefix_security")]
    pub security: Security,
    #[serde(flatten, with = "prefix_security_certificate1")]
    pub security_certificate1: SecurityCertificate,
    #[serde(flatten, with = "prefix_security_certificate2")]
    pub security_certificate2: SecurityCertificate,
    #[serde(flatten, with = "prefix_service_module")]
    pub service_module: ServiceModule,
    #[serde(flatten, with = "prefix_serial")]
    pub serial: Serial,
    #[serde(flatten, with = "prefix_server_boot")]
    pub server_boot: ServerBoot,
    #[serde(flatten, with = "prefix_sys_info")]
    pub sys_info: SysInfo,
    #[serde(flatten, with = "prefix_sys_log")]
    pub sys_log: SysLog,
    #[serde(flatten, with = "prefix_support_assist")]
    pub support_assist: SupportAssist,
    #[serde(flatten, with = "prefix_time")]
    pub time: Time,
    #[serde(flatten, with = "prefix_update")]
    pub update: Update,
    #[serde(flatten, with = "prefix_virtual_console")]
    pub virtual_console: VirtualConsole,
    #[serde(flatten, with = "prefix_virtual_media")]
    pub virtual_media: VirtualMedia,
    #[serde(flatten, with = "prefix_vnc_server")]
    pub vnc_server: VncServer,
    #[serde(flatten, with = "prefix_web_server")]
    pub web_server: WebServer,

    #[serde(flatten, with = "prefix_users1")]
    pub users1: Users,
    #[serde(flatten, with = "prefix_users2")]
    pub users2: Users,
    #[serde(flatten, with = "prefix_users3")]
    pub users3: Users,
    #[serde(flatten, with = "prefix_users4")]
    pub users4: Users,
    #[serde(flatten, with = "prefix_users5")]
    pub users5: Users,
    #[serde(flatten, with = "prefix_users6")]
    pub users6: Users,
    #[serde(flatten, with = "prefix_users7")]
    pub users7: Users,
    #[serde(flatten, with = "prefix_users8")]
    pub users8: Users,
    #[serde(flatten, with = "prefix_users9")]
    pub users9: Users,
    #[serde(flatten, with = "prefix_users10")]
    pub users10: Users,
    #[serde(flatten, with = "prefix_users11")]
    pub users11: Users,
    #[serde(flatten, with = "prefix_users12")]
    pub users12: Users,
    #[serde(flatten, with = "prefix_users13")]
    pub users13: Users,
    #[serde(flatten, with = "prefix_users14")]
    pub users14: Users,
    #[serde(flatten, with = "prefix_users15")]
    pub users15: Users,
    #[serde(flatten, with = "prefix_users16")]
    pub users16: Users,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct AttributesResult {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub attributes: Attributes,
    pub description: String,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ShareParameters {
    pub target: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SystemConfiguration {
    pub shutdown_type: String,
    pub share_parameters: ShareParameters,
    pub import_buffer: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct StorageCollection {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub description: Option<String>,
    pub members: Vec<ODataId>,
    pub name: String,
}

#[cfg(test)]
mod test {
    #[test]
    fn test_bios_parser() {
        let test_data = include_str!("../testdata/bios_dell.json");
        let result: super::Bios = serde_json::from_str(test_data).unwrap();
        println!("result: {result:#?}");
    }
}
