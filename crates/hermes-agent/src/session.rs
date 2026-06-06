use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SessionContext {
    pub working_dir: PathBuf,
    pub session_id: String,
}

impl SessionContext {
    pub fn current_shell() -> Self {
        Self {
            working_dir: std::env::current_dir().unwrap_or_default(),
            session_id: "shell".into(),
        }
    }
}
