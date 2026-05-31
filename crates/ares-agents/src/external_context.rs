use std::cell::RefCell;

thread_local! {
    static CURRENT_ERUKA_CONTEXT: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn get_current_eruka_context() -> Option<String> {
    CURRENT_ERUKA_CONTEXT.with(|ctx| ctx.borrow().clone())
}

pub fn set_current_eruka_context(context: Option<String>) {
    CURRENT_ERUKA_CONTEXT.with(|ctx| *ctx.borrow_mut() = context);
}
