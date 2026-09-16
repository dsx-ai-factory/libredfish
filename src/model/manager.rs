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
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::oem::ManagerExtensions;
use crate::model::{ODataId, ODataLinks};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Managers {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub description: Option<String>,
    pub members: Vec<ODataId>,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Manager {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub actions: Option<Action>,
    pub command_shell: Option<Commandshell>,
    pub date_time: Option<DateTime<Utc>>,
    pub description: Option<String>,
    pub ethernet_interfaces: Option<ODataId>,
    pub firmware_version: Option<String>,
    pub graphical_console: Option<Commandshell>,
    pub id: String,
    pub log_services: Option<ODataId>,
    pub manager_type: Option<String>,
    pub model: Option<String>,
    pub name: String,
    pub network_protocol: Option<ODataId>,
    pub status: Option<Status>,
    #[serde(rename = "UUID")]
    pub uuid: Option<String>,
    pub oem: Option<ManagerExtensions>,
    pub links: Option<ManagerLinks>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct ManagerLinks {
    #[serde(default)]
    pub manager_for_servers: Vec<ODataId>,
    #[serde(default)]
    pub manager_for_chassis: Vec<ODataId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ActionsManagerReset {
    pub target: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Action {
    #[serde(rename = "#Manager.Reset")]
    pub manager_reset: ActionsManagerReset,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Commandshell {
    pub connect_types_supported: Vec<String>,
    pub enabled: Option<bool>,
    pub max_concurrent_sessions: i64,
    pub service_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Status {
    pub state: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Availableaction {
    pub action: String,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub enum ManagerResetType {
    GracefulRestart,
    ForceRestart,
}

impl fmt::Display for ManagerResetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod test {
    use chrono::{Datelike, Timelike};

    use crate::model::oem::{dell, hpe};

    #[test]
    fn test_manager_parser_hp() {
        let test_data = include_str!("testdata/manager_hp.json");
        let result: hpe::Manager = serde_json::from_str(test_data).unwrap();
        println!("result: {result:#?}");
    }

    #[test]
    fn test_manager_parser_dell() {
        let test_data2 = include_str!("testdata/manager_dell.json");
        let m: super::Manager = serde_json::from_str(test_data2).unwrap();
        assert!(m.oem.is_some());
        let oem = m.oem.unwrap();
        assert!(oem.dell.is_some());
        assert!(oem.lenovo.is_none());
        // Verify DateTime field is parsed correctly
        assert!(m.date_time.is_some());
        let dt = m.date_time.unwrap();
        assert_eq!(dt.year(), 2022);
        assert_eq!(dt.month(), 11);
        assert_eq!(dt.day(), 30);
    }

    #[test]
    fn test_manager_parser_lenovo() {
        let test_data2 = include_str!("testdata/manager_lenovo.json");
        let m: super::Manager = serde_json::from_str(test_data2).unwrap();
        assert!(m.oem.is_some());
        let oem = m.oem.unwrap();
        assert!(oem.dell.is_none());
        assert!(oem.lenovo.is_some());
        if let Some(lenovo) = oem.lenovo {
            assert_eq!(lenovo.recipients_settings.retry_count, 5);
        }
        // Verify DateTime field is parsed correctly
        assert!(m.date_time.is_some());
        let dt = m.date_time.unwrap();
        assert_eq!(dt.year(), 2023);
        assert_eq!(dt.month(), 2);
        assert_eq!(dt.day(), 7);
    }

    #[test]
    fn test_manager_parser_dell_attrs() {
        let test_data3 = include_str!("testdata/manager_dell_attrs.json");
        let result3: dell::AttributesResult = serde_json::from_str(test_data3).unwrap();
        println!("result3: {result3:#?}");
    }

    #[test]
    fn test_manager_datetime_parsing() {
        // Test parsing RFC3339 datetime format with timezone offset
        // The input has -06:00 offset, which gets converted to UTC
        let json_data = include_str!("testdata/manager_datetime_test.json");

        let manager: super::Manager = serde_json::from_str(json_data).unwrap();
        assert!(manager.date_time.is_some());

        let dt = manager.date_time.unwrap();
        // Input: 2025-12-05T21:07:58-06:00
        // UTC:   2025-12-06T03:07:58+00:00 (21:07 + 6 hours = 03:07 next day)
        assert_eq!(dt.year(), 2025);
        assert_eq!(dt.month(), 12);
        assert_eq!(dt.day(), 6); // Next day in UTC
        assert_eq!(dt.hour(), 3); // 21 + 6 = 27 -> 3 (next day)
        assert_eq!(dt.minute(), 7);
        assert_eq!(dt.second(), 58);

        // Verify the datetime is converted to UTC format
        let dt_str = dt.to_rfc3339();
        assert_eq!(dt_str, "2025-12-06T03:07:58+00:00");
    }
}
