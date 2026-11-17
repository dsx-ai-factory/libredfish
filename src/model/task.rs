use serde::{Deserialize, Serialize};

use super::ODataLinks;

/// http://redfish.dmtf.org/schemas/v1/Task.v1_7_1.json#/definitions/Task
/// The Task schema contains information about a task that the Redfish task service schedules or executes.
/// Tasks represent operations that take more time than a client typically wants to wait.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Task {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub id: String,
    #[serde(default)]
    pub messages: Vec<super::Message>,
    pub name: Option<String>,
    pub task_state: Option<TaskState>,
    pub task_status: Option<String>,
    pub task_monitor: Option<String>,
    pub percent_complete: Option<u32>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub payload: Option<TaskPayload>,
    pub hide_payload: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TaskPayload {
    pub http_headers: Option<Vec<String>>,
    pub http_operation: Option<String>,
    pub target_uri: Option<String>,
    /// JSON-encoded request body, represented as a string on the wire.
    pub json_body: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum TaskState {
    New,
    Starting,
    Running,
    Suspended,
    Interrupted,
    Pending,
    Stopping,
    Completed,
    Killed,
    Exception,
    Service,
    Cancelling,
    Cancelled,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::Task;
    use serde_json::{json, Value};

    #[test]
    fn simple_update_metadata() {
        // BF-25.10-24 captures; image URI replaced, headers and progress messages removed.
        for (input, state) in [
            (
                include_str!("testdata/simple_update_running.json"),
                "Running",
            ),
            (
                include_str!("testdata/simple_update_completed.json"),
                "Completed",
            ),
        ] {
            let task: Task = serde_json::from_str(input).unwrap();
            assert_eq!(task.task_state.as_ref().unwrap().to_string(), state);
            assert_eq!(
                task.start_time.as_deref(),
                Some("2026-11-10T17:42:10+00:00")
            );
            assert_eq!(task.hide_payload, Some(false));
            assert_eq!(
                task.end_time.as_deref(),
                if state == "Completed" {
                    Some("2026-11-10T17:46:39+00:00")
                } else {
                    None
                }
            );
            let payload = task.payload.as_ref().unwrap();
            assert_eq!(payload.http_operation.as_deref(), Some("POST"));
            assert_eq!(
                payload.target_uri.as_deref(),
                Some("/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate")
            );
            let body: Value = serde_json::from_str(payload.json_body.as_deref().unwrap()).unwrap();
            assert_eq!(
                body["ImageURI"],
                "http://firmware.example.com/images/dpu-os.bfb"
            );
            assert_eq!(
                body["Targets"],
                json!(["redfish/v1/UpdateService/FirmwareInventory/DPU_OS"])
            );
            assert_eq!(body["TransferProtocol"], "HTTP");
            let original: Value = serde_json::from_str(input).unwrap();
            let serialized = serde_json::to_value(&task).unwrap();
            for field in ["StartTime", "EndTime", "HidePayload"] {
                assert_eq!(serialized[field], original[field], "{field}");
            }
            for field in ["HttpOperation", "TargetUri", "JsonBody", "HttpHeaders"] {
                assert_eq!(
                    serialized["Payload"][field], original["Payload"][field],
                    "{field}"
                );
            }
        }
    }

    #[test]
    fn optional_task_metadata() {
        for input in [
            json!({"@odata.type": "#Task.v1_4_3.Task", "@odata.id": "/redfish/v1/TaskService/Tasks/0", "Id": "0", "TaskState": "New"}),
            json!({"@odata.type": "#Task.v1_4_3.Task", "@odata.id": "/redfish/v1/TaskService/Tasks/0", "Id": "0", "StartTime": null, "EndTime": null, "Payload": null, "HidePayload": null}),
            json!({"@odata.type": "#Task.v1_4_3.Task", "@odata.id": "/redfish/v1/TaskService/Tasks/0", "Id": "0", "HidePayload": true}),
            json!({"@odata.type": "#Task.v1_4_3.Task", "@odata.id": "/redfish/v1/TaskService/Tasks/0", "Id": "0", "Payload": {}}),
            json!({"@odata.type": "#Task.v1_4_3.Task", "@odata.id": "/redfish/v1/TaskService/Tasks/0", "Id": "0", "Payload": {"HttpHeaders": null}}),
        ] {
            let task: Task = serde_json::from_value(input.clone()).unwrap();
            assert!(task.start_time.is_none());
            assert!(task.end_time.is_none());
            assert_eq!(task.hide_payload, input["HidePayload"].as_bool());
            if let Some(payload) = task.payload {
                assert!(payload.http_headers.is_none());
                assert!(payload.http_operation.is_none());
                assert!(payload.target_uri.is_none());
                assert!(payload.json_body.is_none());
            } else {
                assert!(input["Payload"].is_null());
            }
        }
    }

    #[test]
    fn task_payload_headers() {
        for headers in [
            json!([]),
            json!(["Content-Type: application/json", "Accept: application/json"]),
        ] {
            let mut input: Value =
                serde_json::from_str(include_str!("testdata/simple_update_running.json")).unwrap();
            input["Payload"]["HttpHeaders"] = headers.clone();
            let task: Task = serde_json::from_value(input).unwrap();
            let expected: Vec<String> = serde_json::from_value(headers.clone()).unwrap();
            assert_eq!(
                task.payload.as_ref().unwrap().http_headers.as_ref(),
                Some(&expected)
            );
            assert_eq!(
                serde_json::to_value(&task).unwrap()["Payload"]["HttpHeaders"],
                headers
            );
        }
    }
}
