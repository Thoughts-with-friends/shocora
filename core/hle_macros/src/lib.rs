//! `#[hle_export]`: turns a plain Rust function into a Cafe OS HLE export.
//!
//! ```ignore
//! #[hle_export(lib = "coreinit", name = "OSGetTime")]
//! fn os_get_time(ctx: &mut HleContext<'_>) -> i64 { ... }
//! ```
//!
//! expands to the function itself plus
//! `pub(crate) const OS_GET_TIME: HleExport`, whose thunk reads the guest
//! arguments from registers (PowerPC SysV ABI) via `FromArg`, calls the
//! function and writes the result back via `IntoReturn`. The expansion refers
//! to `crate::hle`, so the macro is meant for `wiiu_cafe_os` only.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ #[hle_export] fn   │   │ lib.rs                   │   │ registered OS      │
//! │                    │──►│ ──────────────────────── │──►│ function           │
//! │                    │   │ export macro             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! A compile-time helper: marks a Rust function as an OS function the game can call. Terms such
//! as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, LitStr, Pat, parse_macro_input, spanned::Spanned};

#[proc_macro_attribute]
pub fn hle_export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut library: Option<LitStr> = None;
    let mut name: Option<LitStr> = None;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("lib") {
            library = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("name") {
            name = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("expected `lib = \"...\"` or `name = \"...\"`"))
        }
    });
    parse_macro_input!(attr with parser);
    let function = parse_macro_input!(item as ItemFn);

    let Some(library) = library else {
        return syn::Error::new(function.sig.span(), "missing `lib = \"...\"`")
            .to_compile_error()
            .into();
    };
    let fn_name = &function.sig.ident;
    let name = name.unwrap_or_else(|| LitStr::new(&fn_name.to_string(), fn_name.span()));
    let const_name = format_ident!("{}", fn_name.to_string().to_uppercase());

    // Every parameter after the context is a guest argument.
    let mut idents = Vec::new();
    let mut types = Vec::new();
    for (index, input) in function.sig.inputs.iter().enumerate().skip(1) {
        match input {
            FnArg::Typed(typed) => {
                let ident = match &*typed.pat {
                    Pat::Ident(p) => p.ident.clone(),
                    _ => format_ident!("arg{}", index),
                };
                idents.push(format_ident!("__{}", ident));
                types.push((*typed.ty).clone());
            }
            FnArg::Receiver(r) => {
                return syn::Error::new(r.span(), "HLE exports cannot take self")
                    .to_compile_error()
                    .into();
            }
        }
    }

    quote! {
        #[allow(clippy::needless_pass_by_ref_mut, reason = "HLE thunks always pass the context mutably")]
        #function

        #[doc = concat!("HLE export `", #library, "::", #name, "`.")]
        pub(crate) const #const_name: crate::hle::HleExport = crate::hle::HleExport {
            library: #library,
            name: #name,
            function: {
                fn thunk(ctx: &mut crate::hle::HleContext<'_>) -> crate::hle::HleResult {
                    let mut args = crate::hle::ArgReader::new();
                    #( let #idents = <#types as crate::hle::FromArg>::from_arg(ctx, &mut args)?; )*
                    crate::hle::IntoReturn::write_return(#fn_name(ctx, #(#idents),*), ctx)
                }
                thunk
            },
        };
    }
    .into()
}
