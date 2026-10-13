//! Source-scoped routing for audited native instrumentation.
//!
//! Unlike `tracing::dispatcher::with_default`, this never changes tracing's
//! process-wide "a subscriber has ever been set" latch or legacy-log fallback.
//! Compiled only with `wrapper-tracing`, independently of ordinary client tracing.
use std::cell::RefCell;
pub use tracing;
/// A source-local collector, never registered with tracing's global callsite registry.
pub type NativeDispatch = std::sync::Arc<dyn tracing::Subscriber + Send + Sync>;
thread_local! { static CURRENT: RefCell<Option<NativeDispatch>> = const { RefCell::new(None) }; }
#[must_use]
pub fn current() -> Option<NativeDispatch> {
    CURRENT
        .try_with(|value| value.borrow().clone())
        .ok()
        .flatten()
}
pub fn with_dispatch<T>(dispatch: Option<&NativeDispatch>, call: impl FnOnce() -> T) -> T {
    struct Restore(Option<NativeDispatch>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = CURRENT.try_with(|value| {
                value.replace(self.0.take());
            });
        }
    }
    // Host TLS owners can finalize clients after this routing slot has been destroyed.
    // Their cleanup must still run, even when no source-local route can be installed.
    let _restore = CURRENT
        .try_with(|value| value.replace(dispatch.cloned()))
        .ok()
        .map(Restore);
    call()
}
/// Private metadata identity; source-routed events do not register ambient callsites.
pub struct EventCallsite(pub &'static tracing::Metadata<'static>);
impl tracing::callsite::Callsite for EventCallsite {
    fn set_interest(&self, _: tracing::subscriber::Interest) {}
    fn metadata(&self) -> &tracing::Metadata<'_> {
        self.0
    }
}

/// Emit to an explicit native route, or retain the original ambient tracing behavior.
/// This intentionally supports named events with scalar, Debug, or Display fields.
#[macro_export]
macro_rules! scoped_event {
    (name: $name:expr, target: $target:expr, $level:path, $($fields:tt)*) => {{
        if let Some(dispatch) = $crate::native_tracing::current() {
            if $level <= $crate::native_tracing::tracing::level_filters::STATIC_MAX_LEVEL {
                $crate::__native_event_fields!(@parse dispatch, $name, $target, $level; []; []; $($fields)*);
            }
        } else {
            $crate::native_tracing::tracing::event!(name: $name, target: $target, $level, { $($fields)* });
        }
    }};
}
#[doc(hidden)]
#[macro_export]
macro_rules! __native_event_fields {
    (@parse $dispatch:ident, $name:expr, $target:expr, $level:path; [$($names:expr,)*]; [$($values:expr,)*];) => {{
        static META: $crate::native_tracing::tracing::Metadata<'static> = $crate::native_tracing::tracing::Metadata::new(
            $name, $target, $level, Some(file!()), Some(line!()), Some(module_path!()),
            $crate::native_tracing::tracing::field::FieldSet::new(&[$($names,)*],
                $crate::native_tracing::tracing::callsite::Identifier(&SITE)),
            $crate::native_tracing::tracing::metadata::Kind::EVENT);
        static SITE: $crate::native_tracing::EventCallsite = $crate::native_tracing::EventCallsite(&META);
        if $dispatch.enabled(&META) {
            // One expression keeps nested Debug/Display and value temporaries alive.
            $dispatch.event(&$crate::native_tracing::tracing::Event::new(&META,
                &META.fields().value_set_all(&[$(Some($values),)*])));
        }
    }};
    (@parse $dispatch:ident, $name:expr, $target:expr, $level:path; [$($names:expr,)*]; [$($values:expr,)*]; $field:ident = ? $value:expr, $($rest:tt)*) => {
        $crate::__native_event_fields!(@parse $dispatch, $name, $target, $level; [$($names,)* stringify!($field),]; [$($values,)* &$crate::native_tracing::tracing::field::debug(&$value),]; $($rest)*);
    };
    (@parse $dispatch:ident, $name:expr, $target:expr, $level:path; [$($names:expr,)*]; [$($values:expr,)*]; $field:ident = % $value:expr, $($rest:tt)*) => {
        $crate::__native_event_fields!(@parse $dispatch, $name, $target, $level; [$($names,)* stringify!($field),]; [$($values,)* &$crate::native_tracing::tracing::field::display(&$value),]; $($rest)*);
    };
    (@parse $dispatch:ident, $name:expr, $target:expr, $level:path; [$($names:expr,)*]; [$($values:expr,)*]; $field:ident = $value:expr, $($rest:tt)*) => {
        $crate::__native_event_fields!(@parse $dispatch, $name, $target, $level; [$($names,)* stringify!($field),]; [$($values,)* &$value,]; $($rest)*);
    };
    (@parse $dispatch:ident, $name:expr, $target:expr, $level:path; [$($names:expr,)*]; [$($values:expr,)*]; $field:ident, $($rest:tt)*) => {
        $crate::__native_event_fields!(@parse $dispatch, $name, $target, $level; [$($names,)* stringify!($field),]; [$($values,)* &$field,]; $($rest)*);
    };
    // Normalize a final field without a trailing comma.
    (@parse $dispatch:ident, $name:expr, $target:expr, $level:path; [$($names:expr,)*]; [$($values:expr,)*]; $($last:tt)+) => {
        $crate::__native_event_fields!(@parse $dispatch, $name, $target, $level; [$($names,)*]; [$($values,)*]; $($last)+,);
    };
}
/// Keep host span behavior, while reducing scoped native spans to one owned record.
#[macro_export]
macro_rules! scoped_info_span {
    (name: $name:expr, target: $target:expr, $($fields:tt)*) => {{
        if $crate::native_tracing::current().is_some() {
            $crate::scoped_event!(name: $name, target: $target, $crate::native_tracing::tracing::Level::INFO, $($fields)*);
            $crate::native_tracing::tracing::Span::none()
        } else {
            $crate::native_tracing::tracing::info_span!(target: $target, $name, $($fields)*)
        }
    }};
}
