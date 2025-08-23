use crate::OrchestrationContext;

#[derive(Debug, Clone)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

pub fn push_buffer(ctx: &OrchestrationContext, level: LogLevel, msg: String) {
    ctx.push_log(level, msg);
}

#[macro_export]
macro_rules! durable_info {
    ($ctx:expr, $($arg:tt)+) => {{
        $crate::logging::push_buffer(&$ctx, $crate::logging::LogLevel::Info, format!($($arg)+));
    }};
}

#[macro_export]
macro_rules! durable_warn {
    ($ctx:expr, $($arg:tt)+) => {{
        $crate::logging::push_buffer(&$ctx, $crate::logging::LogLevel::Warn, format!($($arg)+));
    }};
}

#[macro_export]
macro_rules! durable_error {
    ($ctx:expr, $($arg:tt)+) => {{
        $crate::logging::push_buffer(&$ctx, $crate::logging::LogLevel::Error, format!($($arg)+));
    }};
}
