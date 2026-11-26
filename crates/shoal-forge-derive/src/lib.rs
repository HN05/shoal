//! Attribute macros that write the forwarding and extension traits of
//! `shoal-forge` and `shoal-forge-types`.
use proc_macro::TokenStream;
use quote::quote;
use syn::{Error, ItemImpl, ItemTrait, parse_macro_input};

mod extension;
mod forwarded;

/// `#[forwarded(Kind, dispatch)]` on a trait implements it for `Kind`, a
/// type that names one of the trait's implementations. Each method calls
/// the same method of the implementation `dispatch!(self, backend => body)`
/// binds to `backend`, so a new method of the trait needs no forwarder.
#[proc_macro_attribute]
pub fn forwarded(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let target = parse_macro_input!(attribute as forwarded::Target);
    let definition = parse_macro_input!(item as ItemTrait);
    let implementation = target
        .implementation(&definition)
        .unwrap_or_else(Error::into_compile_error);
    quote!(#definition #implementation).into()
}

/// `#[extension(pub Name)]` on an inherent `impl Type` declares the trait
/// `Name` with each method's signature and documentation, and implements it
/// for `Type`. A crate adds methods to another crate's type this way.
/// `#[extension(pub Name, pub(crate) Steps)]` puts the methods the impl
/// marks `pub` in `Name` and the others in `Steps`.
#[proc_macro_attribute]
pub fn extension(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let traits = parse_macro_input!(attribute as extension::Traits);
    let implementation = parse_macro_input!(item as ItemImpl);
    traits
        .declare(implementation)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}
