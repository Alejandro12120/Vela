use std::rc::Rc;

use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate};

pub(crate) fn binding<A: Action>(
    cx: &App,
    keystrokes: &str,
    action: A,
    context: Option<&str>,
) -> KeyBinding {
    KeyBinding::load(
        keystrokes,
        Box::new(action),
        context.map(|context| {
            Rc::new(KeyBindingContextPredicate::parse(context).expect("valid key context"))
        }),
        false,
        None,
        cx,
    )
    .expect("valid key binding")
}

pub(crate) const fn shortcut(mac: &'static str, other: &'static str) -> &'static str {
    #[cfg(target_os = "macos")]
    {
        mac
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mac;
        other
    }
}
