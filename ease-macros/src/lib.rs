//! Proc Macros for EASE
use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn profile(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let name = input.sig.ident.to_string();
    let attrs = input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;

    let expanded = quote!(
        #(#attrs)*
        #vis #sig {
            // `module_path!()` and `concat!` resolve at the call site, so the
            // resulting `&'static str` is the full module path of where this
            // `#[profile]` was applied, joined to the function name.
            let _g = crate::kernel::profile::ProfileGuard::new(
                concat!(module_path!(), "::", #name)
            );
            #block
        }
    );

    expanded.into()
}

// Trace can be used on lock-entry-point functions
// Manually use `snapshot_raw` for snapshots under lock
#[proc_macro_attribute]
pub fn trace(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let name = input.sig.ident.to_string();
    let attrs = input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;

    let expanded = quote!(
        #(#attrs)*
        #vis #sig {
        // `module_path!()` and `concat!` resolve at the call site, so the
        // resulting `&'static str` is the full module path of where this
        // `#[profile]` was applied, joined to the function name.
        crate::kernel::sched::trace::take_snapshot(#name);
        #block
        }
    );

    expanded.into()
}
