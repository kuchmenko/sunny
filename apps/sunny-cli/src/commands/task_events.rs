#[derive(Debug, Clone)]
pub enum TaskProgressEvent {
    Started {
        task_id: String,
        title: String,
    },
    Completed {
        task_id: String,
        title: String,
        summary: String,
    },
    Failed {
        task_id: String,
        title: String,
        error: String,
    },
    Suspended {
        task_id: String,
        title: String,
        children_count: usize,
    },
    Requeued {
        task_id: String,
        title: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_progress_event_variants_compile() {
        let _ = TaskProgressEvent::Started {
            task_id: "1".to_string(),
            title: "t".to_string(),
        };
        let _ = TaskProgressEvent::Completed {
            task_id: "1".to_string(),
            title: "t".to_string(),
            summary: "s".to_string(),
        };
        let _ = TaskProgressEvent::Failed {
            task_id: "1".to_string(),
            title: "t".to_string(),
            error: "e".to_string(),
        };
        let _ = TaskProgressEvent::Suspended {
            task_id: "1".to_string(),
            title: "t".to_string(),
            children_count: 1,
        };
        let _ = TaskProgressEvent::Requeued {
            task_id: "1".to_string(),
            title: "t".to_string(),
        };
    }
}
