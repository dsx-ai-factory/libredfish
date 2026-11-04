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

impl Task {
    /// The Job URI an update Task hands its work to, from its
    /// `Update.<version>.OperationTransitionedToJob` message.
    ///
    /// A MessageId is `RegistryName.MajorVersion.MinorVersion[.ErrataVersion].MessageKey`,
    /// so the version is matched loosely: Lenovo XCC3 reports
    /// `Update.1.0.1.OperationTransitionedToJob`. Once a Task transitions, its own
    /// state stops tracking the update (the XCC3 Task is `Completed` while its Job
    /// is still writing the image), so the Job must be followed instead.
    pub fn transitioned_job_uri(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|m| is_operation_transitioned_to_job(&m.message_id))
            .and_then(|m| m.message_args.first())
            .map(String::as_str)
    }
}

fn is_operation_transitioned_to_job(message_id: &str) -> bool {
    let mut parts = message_id.split('.');
    let (Some("Update"), Some("OperationTransitionedToJob")) = (parts.next(), parts.next_back())
    else {
        return false;
    };
    let version: Vec<&str> = parts.collect();
    matches!(version.len(), 2 | 3)
        && version
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XCC3_JOB: &str = "/redfish/v1/JobService/Jobs/FW_Update_1";

    /// Task 5 of a Lenovo SR680a V4 XCC3 (IHX426W 1.94) after a multipart XCC
    /// push, as the BMC served it: `Completed` six seconds after the upload,
    /// while `FW_Update_1` still had the image to write.
    const XCC3_TASK: &str = r##"{
      "@odata.context": "/redfish/v1/$metadata#Task.Task",
      "@odata.etag": "W/\"1495A231\"",
      "@odata.id": "/redfish/v1/TaskService/Tasks/5",
      "@odata.type": "#Task.v1_7_4.Task",
      "Description": "This resource represents a task for a Redfish implementation.",
      "EndTime": "2026-11-04T22:00:27+00:00",
      "HidePayload": true,
      "Id": "5",
      "Messages": [
        {
          "@odata.type": "#Message.v1_1_1.Message",
          "Message": "The update operation has transitioned to the job at URI '/redfish/v1/JobService/Jobs/FW_Update_1'.",
          "MessageArgs": ["/redfish/v1/JobService/Jobs/FW_Update_1"],
          "MessageId": "Update.1.0.1.OperationTransitionedToJob",
          "MessageSeverity": "OK",
          "Resolution": "Follow the referenced job and monitor the job for further updates."
        }
      ],
      "Name": "Task 5",
      "PercentComplete": 100,
      "StartTime": "2026-11-04T22:00:21+00:00",
      "TaskMonitor": "/redfish/v1/TaskService/TaskMonitors/5",
      "TaskState": "Completed",
      "TaskStatus": "OK"
    }"##;

    #[test]
    fn xcc3_task_follows_its_job() {
        let task: Task = serde_json::from_str(XCC3_TASK).unwrap();
        assert_eq!(task.transitioned_job_uri(), Some(XCC3_JOB));
    }

    #[test]
    fn transitioned_job_uri_by_message_id() {
        let mut task: Task = serde_json::from_str(XCC3_TASK).unwrap();
        for (message_id, args, expected) in [
            (
                "Update.1.0.OperationTransitionedToJob",
                &[XCC3_JOB][..],
                Some(XCC3_JOB),
            ),
            (
                "Update.1.2.OperationTransitionedToJob",
                &[XCC3_JOB][..],
                Some(XCC3_JOB),
            ),
            // No URI to follow: keep the Task's own state.
            ("Update.1.0.1.OperationTransitionedToJob", &[][..], None),
            ("Update.1.0.1.UpdateSuccessful", &[XCC3_JOB][..], None),
            ("Base.1.0.OperationTransitionedToJob", &[XCC3_JOB][..], None),
            ("Update.OperationTransitionedToJob", &[XCC3_JOB][..], None),
            (
                "Update.1.x.OperationTransitionedToJob",
                &[XCC3_JOB][..],
                None,
            ),
        ] {
            task.messages[0].message_id = message_id.to_string();
            task.messages[0].message_args = args.iter().map(|a| a.to_string()).collect();
            assert_eq!(task.transitioned_job_uri(), expected, "{message_id}");
        }
    }
}
