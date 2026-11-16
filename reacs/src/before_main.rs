//! Runs the registration code that reacs's macros generate before `main`.
//!
//! `#[component]`, `#[signal]`, `#[resource]` and `#[system]` wrap their registration function in
//! `__before_main!`, which puts a pointer to it in the platform's initializer section, as `ctor`'s
//! `naked` constructors do. Each platform gets its own definition of `__before_main!`, picked by a
//! `#[cfg]` here, so the code the macros expand to in user crates holds no `#[cfg]` at all.
//! rust-analyzer greys out every inactive `#[cfg]` branch in macro output and reports it at the
//! macro's line, so `ctor`'s per-platform branches showed "code is inactive due to #[cfg]
//! directives: target_os = "uefi" is disabled" on every `#[component]` in user code.

/// Defines `fn $name` and puts a pointer to it in the linker section `$section`, whose functions
/// the platform runs before `main`.
#[doc(hidden)]
#[macro_export]
macro_rules! __before_main_in {
    ($section:literal, $(#[$attr:meta])* fn $name:ident() $body:block) => {
        $(#[$attr])*
        fn $name() $body

        #[allow(unsafe_code)]
        const _: () = {
            #[used]
            #[unsafe(link_section = $section)]
            static ENTRY: extern "C" fn() = {
                extern "C" fn entry() {
                    $name()
                }
                entry
            };
        };
    };
}

/// Runs `fn $name` before `main` (Apple targets).
#[cfg(target_vendor = "apple")]
#[doc(hidden)]
#[macro_export]
macro_rules! __before_main {
    ($($item:tt)*) => {
        $crate::__before_main_in!("__DATA,__mod_init_func,mod_init_funcs", $($item)*);
    };
}

/// Runs `fn $name` before `main` (Windows).
#[cfg(windows)]
#[doc(hidden)]
#[macro_export]
macro_rules! __before_main {
    ($($item:tt)*) => {
        $crate::__before_main_in!(".CRT$XCU", $($item)*);
    };
}

/// Runs `fn $name` before `main` (Linux, the BSDs, Android, wasm and other ELF-style targets).
#[cfg(not(any(target_vendor = "apple", windows)))]
#[doc(hidden)]
#[macro_export]
macro_rules! __before_main {
    ($($item:tt)*) => {
        $crate::__before_main_in!(".init_array", $($item)*);
    };
}
