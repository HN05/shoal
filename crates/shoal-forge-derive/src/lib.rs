//! `#[forwarded(Kind, dispatch)]` on a trait implements it for `Kind`, a type
//! that names one of the trait's implementations: each method runs the same
//! method of the implementation `dispatch!(self, name => body)` binds to
//! `name`, so adding a method to the trait needs no forwarder.
use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::{
    Error, FnArg, Ident, ItemTrait, Pat, Path, Result, Signature, Token, TraitItem,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

#[proc_macro_attribute]
pub fn forwarded(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let target = parse_macro_input!(attribute as Target);
    let definition = parse_macro_input!(item as ItemTrait);
    let implementation = target
        .implementation(&definition)
        .unwrap_or_else(Error::into_compile_error);
    quote!(#definition #implementation).into()
}

/// The type the trait is implemented for and the macro that dispatches on it.
struct Target {
    kind: Path,
    dispatch: Path,
}

impl Parse for Target {
    fn parse(input: ParseStream) -> Result<Self> {
        let kind = input.parse()?;
        input.parse::<Token![,]>()?;
        let dispatch = input.parse()?;
        Ok(Self { kind, dispatch })
    }
}

impl Target {
    fn implementation(&self, definition: &ItemTrait) -> Result<Tokens> {
        let forwarders = definition
            .items
            .iter()
            .map(|item| match item {
                TraitItem::Fn(method) => self.forwarder(&method.sig),
                other => Err(Error::new_spanned(
                    other,
                    "a forwarded trait has only methods",
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        let name = &definition.ident;
        let kind = &self.kind;
        let (generics, arguments, bounds) = definition.generics.split_for_impl();
        Ok(quote! {
            impl #generics #name #arguments for #kind #bounds {
                #(#forwarders)*
            }
        })
    }

    /// The method `signature` declares, calling the same method of the
    /// implementation `self` names.
    fn forwarder(&self, signature: &Signature) -> Result<Tokens> {
        let name = &signature.ident;
        let arguments = arguments(signature)?;
        let awaited = signature.asyncness.map(|_| quote!(.await));
        let dispatch = &self.dispatch;
        Ok(quote! {
            #signature {
                #dispatch!(self, backend => backend.#name(#(#arguments),*) #awaited)
            }
        })
    }
}

/// The names of the method's arguments after its `self` receiver.
fn arguments(signature: &Signature) -> Result<Vec<&Ident>> {
    let mut inputs = signature.inputs.iter();
    if !matches!(inputs.next(), Some(FnArg::Receiver(_))) {
        return Err(Error::new_spanned(
            signature,
            "a forwarded method takes `self`",
        ));
    }
    inputs
        .map(|input| match input {
            FnArg::Typed(typed) => match &*typed.pat {
                Pat::Ident(pattern) => Ok(&pattern.ident),
                other => Err(Error::new_spanned(
                    other,
                    "a forwarded method binds each argument to a name",
                )),
            },
            FnArg::Receiver(receiver) => Err(Error::new_spanned(receiver, "a second `self`")),
        })
        .collect()
}
