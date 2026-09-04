//! Macros for duplicating functions per HART

// The visibility is common across both asm functions
macro_rules! naked_asm_function {
    ($vis: vis, $section0:literal, $name0:ident, $section1:literal, $name1:ident, $body:tt) => {

            #[unsafe(link_section = $section0)]
            #[unsafe(no_mangle)]
            #[unsafe(naked)]
            $vis unsafe extern "C" fn $name0() {
                core::arch::naked_asm! $body;
            }

            #[unsafe(link_section = $section1)]
            #[unsafe(no_mangle)]
            #[unsafe(naked)]
            $vis unsafe extern "C" fn $name1() {
                core::arch::naked_asm! $body;
            }
    };
}

#[rustfmt::skip]
macro_rules! trap_vector {
    ($section0:literal, $name0:ident, $handler0:path, $section1:literal, $name1:ident, $handler1:path, $num_slots:expr, $body:literal) => {
        core::arch::global_asm!(
            concat!(
                ".section ", $section0, ", \"ax\"\n",
                ".global ", stringify!($name0),"\n",
                ".balign 4\n",
                stringify!($name0),":\n",
                $body,
            ),
            handler = sym $handler0,
            num_slots = const $num_slots,
        );

        core::arch::global_asm!(
            concat!(
                ".section ", $section1, ", \"ax\"\n",
                ".global ", stringify!($name1),"\n",
                ".balign 4\n",
                stringify!($name1),":\n",
                $body,
            ),
            handler = sym $handler1,
            num_slots = const $num_slots,
        );
    };
}

pub(crate) use naked_asm_function;
pub(crate) use trap_vector;
