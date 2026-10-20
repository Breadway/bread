//! Which actions the preview page may offer, as plain functions with no GTK
//! in them, so the rules can be unit tested without a display.
//!
//! The slot holds the report that Save and Send act on. Every rebuild empties
//! it first, and a failed rebuild leaves it empty, so a user can never save or
//! send a report from before the latest request.

/// A rebuild has started: whatever was prepared before is no longer valid.
pub fn rebuild_started<T>(slot: &mut Option<T>) {
    *slot = None;
}

/// A rebuild finished. A success replaces the slot; a failure empties it, so
/// nothing is prepared, saved or sent. Returns whether a report is now ready.
pub fn settle<T, E>(slot: &mut Option<T>, built: Result<T, E>) -> bool {
    match built {
        Ok(report) => {
            *slot = Some(report);
            true
        }
        Err(_) => {
            *slot = None;
            false
        }
    }
}

/// `(save, send)` for the preview page. Save needs a prepared report. Send
/// also needs uploads to be switched on.
pub fn actions(ready: bool, upload_enabled: bool) -> (bool, bool) {
    (ready, ready && upload_enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_rebuild_clears_the_previous_report() {
        let mut slot = Some(1u8);
        rebuild_started(&mut slot);
        let ready = settle(&mut slot, Err::<u8, _>("crash marker too large"));
        assert!(!ready);
        assert_eq!(slot, None);
        assert_eq!(actions(slot.is_some(), true), (false, false));
    }

    #[test]
    fn successful_rebuild_replaces_the_report() {
        let mut slot = Some(1u8);
        rebuild_started(&mut slot);
        assert!(settle(&mut slot, Ok::<u8, String>(2)));
        assert_eq!(slot, Some(2));
        assert_eq!(actions(slot.is_some(), true), (true, true));
    }

    #[test]
    fn previous_report_is_not_actionable_while_rebuilding() {
        let mut slot = Some(1u8);
        rebuild_started(&mut slot);
        assert_eq!(actions(slot.is_some(), true), (false, false));
    }

    #[test]
    fn prepared_report_allows_save_and_send_only_when_uploads_are_on() {
        assert_eq!(actions(true, true), (true, true));
        assert_eq!(actions(true, false), (true, false));
    }

    #[test]
    fn nothing_prepared_allows_nothing() {
        assert_eq!(actions(false, true), (false, false));
        assert_eq!(actions(false, false), (false, false));
    }
}
