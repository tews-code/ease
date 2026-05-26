//! Macros for duplicating functions per HART

macro_rules! naked_asm_function {
    ($section0:literal, $name0:ident, $section1:literal, $name1:ident, $body:tt) => {

            #[unsafe(link_section = $section0)]
            #[unsafe(no_mangle)]
            #[unsafe(naked)]
            unsafe extern "C" fn $name0() {
                core::arch::naked_asm! $body;
            }

            #[unsafe(link_section = $section1)]
            #[unsafe(no_mangle)]
            #[unsafe(naked)]
            unsafe extern "C" fn $name1() {
                core::arch::naked_asm! $body;
            }
    };
}

#[rustfmt::skip]
macro_rules! global_asm_function {
    ($section0:literal, $name0:ident, $handler0:path, $section1:literal, $name1:ident, $handler1:path, $body:literal) => {
        core::arch::global_asm!(
            concat!(
                ".section ", $section0, ", \"ax\"\n",
                ".global ", stringify!($name0),"\n",
                ".align 4\n",
                stringify!($name0),":\n",
                $body,
            ),
            handler = sym $handler0,
        );

        core::arch::global_asm!(
            concat!(
                ".section ", $section1, ", \"ax\"\n",
                ".global ", stringify!($name1),"\n",
                ".align 4\n",
                stringify!($name1),":\n",
                $body,
            ),
            handler = sym $handler1,
        );
    };
}

pub(crate) use global_asm_function;
pub(crate) use naked_asm_function;
