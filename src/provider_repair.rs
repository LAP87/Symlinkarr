pub(crate) const SOURCE_MISSING_BEFORE_LINK: &str = "source_missing_before_link";
pub(crate) const SOURCE_UNREADABLE_BEFORE_LINK: &str = "source_unreadable_before_link";
pub(crate) const SOURCE_OR_TARGET_INVALID: &str = "source_or_target_invalid";
pub(crate) const ORPHAN_FILESYSTEM_DEAD_SYMLINK: &str = "orphan_filesystem_dead_symlink";
pub(crate) const REPAIR_FAILED_ACTION: &str = "repair_failed";

pub(crate) fn is_provider_repair_note(note: &str) -> bool {
    matches!(
        note,
        SOURCE_MISSING_BEFORE_LINK
            | SOURCE_UNREADABLE_BEFORE_LINK
            | SOURCE_OR_TARGET_INVALID
            | ORPHAN_FILESYSTEM_DEAD_SYMLINK
    )
}

pub(crate) fn is_provider_repair_action(action: &str) -> bool {
    action == REPAIR_FAILED_ACTION
}
