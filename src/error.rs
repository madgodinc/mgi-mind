use thiserror::Error;

#[derive(Error, Debug)]
pub enum MindError {
    #[error("MGI-Mind is not initialized. Run `mgimind init` first.")]
    NotInitialized,

    #[error("Library '{0}' not found")]
    LibraryNotFound(String),

    #[error("Library '{0}' already exists")]
    LibraryExists(String),
}
