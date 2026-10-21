//! Safe-mode confirmation before sending SQL to the server.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{App, Window};
use quarry_core::model::Confirmation;
use quarry_core::{SafeMode, Session};

use crate::prompt;

/// Runs `run` now, or after the confirmation the connection's safe mode asks for.
pub fn guarded(session: &Session, sql: &str, window: &mut Window, cx: &mut App, run: impl FnOnce(&mut Window, &mut App) + 'static) {
    let mode: SafeMode = session.safe_mode();
    let run = Rc::new(RefCell::new(Some(run)));
    let call = move |window: &mut Window, cx: &mut App| {
        if let Some(f) = run.borrow_mut().take() {
            f(window, cx);
        }
    };
    let preview: String = sql.chars().take(600).collect();
    match mode.check(sql) {
        // Deferred: callers are usually mid-update of the entity `run` updates.
        Confirmation::None => window.defer(cx, call),
        Confirmation::Warn => prompt::confirm(
            "Safe mode",
            &format!("Do you want to send this query to the server?\n\n{preview}"),
            "Execute",
            window,
            cx,
            call,
        ),
        Confirmation::TypeName => {
            let name = session.config.display_name();
            prompt::ask(
                "Safe mode",
                &format!("Type the connection name \"{name}\" to execute:\n\n{preview}"),
                "",
                false,
                window,
                cx,
                move |typed, window, cx| {
                    if typed.trim() == name {
                        call(window, cx);
                    } else {
                        prompt::error("Safe mode", "The connection name does not match. Nothing was executed.", window, cx);
                    }
                },
            );
        }
    }
}
