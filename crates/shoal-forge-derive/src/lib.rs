//! `#[forwarded(Kind, dispatch)]` on a trait implements it for `Kind`, a type
//! that names one of the trait's implementations: each method runs the same
//! method of the implementation `dispatch!(self, name => body)` binds to
//! `name`, so adding a method to the trait needs no forwarder.
//!
//! `#[extension(pub Name)]` on an inherent `impl Type` declares the trait
//! `Name` with the signature and documentation of each of its methods and
//! implements it for `Type` instead, so a crate adds methods to another
//! crate's type that its callers call as methods once they import `Name`.
//! `#[extension(pub Name, pub(crate) Steps)]` declares the methods the impl
//! marks `pub` in `Name` and the others in `Steps`.
use proc_macro::TokenStream;
use quote::quote;
use syn::{Error, ItemImpl, ItemTrait, parse_macro_input};

mod extension;
mod forwarded;

#[proc_macro_attribute]
pub fn forwarded(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let target = parse_macro_input!(attribute as forwarded::Target);
    let definition = parse_macro_input!(item as ItemTrait);
    let implementation = target
        .implementation(&definition)
        .unwrap_or_else(Error::into_compile_error);
    quote!(#definition #implementation).into()
}

#[proc_macro_attribute]
pub fn extension(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let traits = parse_macro_input!(attribute as extension::Traits);
    let implementation = parse_macro_input!(item as ItemImpl);
    traits
        .declare(implementation)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}
