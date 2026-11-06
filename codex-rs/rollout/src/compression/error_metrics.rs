//! Failure labels for rollout reads. Only static stages and bounded I/O kinds
//! are exported; error messages, paths, and rollout contents never become metric tags.

use std::io;

pub(super) fn error_kind(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::WriteZero => "write_zero",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        io::ErrorKind::StorageFull => "storage_full",
        io::ErrorKind::ReadOnlyFilesystem => "read_only_filesystem",
        io::ErrorKind::NotADirectory => "not_a_directory",
        io::ErrorKind::IsADirectory => "is_a_directory",
        _ => "other",
    }
}
