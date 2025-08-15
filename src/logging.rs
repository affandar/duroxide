// macros only; no direct imports needed

#[macro_export]
macro_rules! durable_info {
    ($ctx:expr, $($arg:tt)+) => {{
        if $ctx.is_logging_enabled() {
            ::tracing::info!(turn_idx = $ctx.turn_index(), $($arg)+);
        }
    }};
}

#[macro_export]
macro_rules! durable_warn {
    ($ctx:expr, $($arg:tt)+) => {{
        if $ctx.is_logging_enabled() {
            ::tracing::warn!(turn_idx = $ctx.turn_index(), $($arg)+);
        }
    }};
}

#[macro_export]
macro_rules! durable_error {
    ($ctx:expr, $($arg:tt)+) => {{
        if $ctx.is_logging_enabled() {
            ::tracing::error!(turn_idx = $ctx.turn_index(), $($arg)+);
        }
    }};
}


