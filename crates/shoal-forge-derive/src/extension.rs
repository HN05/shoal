//! Turning an inherent impl into the traits that declare its methods.
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::{
    Attribute, Error, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, Pat, PatWild, Result, Token,
    Type, Visibility,
    parse::{Parse, ParseStream},
};

/// The traits an impl's methods go to: the first takes its `pub` methods
/// and the second, where there is one, the others; else the first takes
/// them all.
pub(crate) struct Traits {
    public: Declared,
    rest: Option<Declared>,
}

/// A trait's visibility and name.
struct Declared {
    visibility: Visibility,
    name: Ident,
}

impl Parse for Declared {
    fn parse(input: ParseStream) -> Result<Self> {
        Ok(Self {
            visibility: input.parse()?,
            name: input.parse()?,
        })
    }
}

impl Parse for Traits {
    fn parse(input: ParseStream) -> Result<Self> {
        let public = input.parse()?;
        let rest = match input.parse::<Option<Token![,]>>()? {
            Some(_) if !input.is_empty() => Some(input.parse()?),
            _ => None,
        };
        Ok(Self { public, rest })
    }
}

impl Traits {
    /// The traits that declare `implementation`'s methods, each implemented
    /// for its type.
    pub(crate) fn declare(self, implementation: ItemImpl) -> Result<Tokens> {
        if let Some((_, path, _)) = &implementation.trait_ {
            return Err(Error::new_spanned(path, "an extension is an inherent impl"));
        }
        if !implementation.generics.params.is_empty() {
            return Err(Error::new_spanned(
                &implementation.generics,
                "an extension impl has no generics",
            ));
        }
        let methods = implementation
            .items
            .into_iter()
            .map(|item| match item {
                ImplItem::Fn(method) => Ok(method),
                other => Err(Error::new_spanned(other, "an extension has only methods")),
            })
            .collect::<Result<Vec<_>>>()?;
        let (public, rest): (Vec<_>, Vec<_>) = match self.rest {
            Some(_) => methods
                .into_iter()
                .partition(|method| matches!(method.vis, Visibility::Public(_))),
            None => (methods, Vec::new()),
        };
        let self_type = &implementation.self_ty;
        let attributes = &implementation.attrs;
        let mut tokens = self.public.extension(attributes, self_type, public)?;
        if let Some(steps) = &self.rest {
            let undocumented: Vec<_> = attributes
                .iter()
                .filter(|attribute| !attribute.path().is_ident("doc"))
                .cloned()
                .collect();
            tokens.extend(steps.extension(&undocumented, self_type, rest)?);
        }
        Ok(tokens)
    }
}

impl Declared {
    /// This trait, carrying `attributes`, with `methods`, and its
    /// implementation for `self_type`.
    fn extension(
        &self,
        attributes: &[Attribute],
        self_type: &Type,
        methods: Vec<ImplItemFn>,
    ) -> Result<Tokens> {
        if methods.is_empty() {
            return Err(Error::new_spanned(
                &self.name,
                "no method of the impl goes to this trait",
            ));
        }
        let Self { visibility, name } = self;
        let declarations: Vec<_> = methods.iter().map(declaration).collect();
        let implemented = methods.into_iter().map(|mut method| {
            method.vis = Visibility::Inherited;
            method
        });
        let applied = attributes
            .iter()
            .filter(|attribute| !attribute.path().is_ident("doc"));
        // Callers call the methods on the one type that implements the
        // trait, whose futures they see whole, so no bound on them is lost.
        Ok(quote! {
            #(#attributes)*
            #[allow(async_fn_in_trait)]
            #visibility trait #name: Sized {
                #(#declarations)*
            }

            #(#applied)*
            impl #name for #self_type {
                #(#implemented)*
            }
        })
    }
}

/// The declaration of `method` in its trait, with its attributes and
/// without patterns, which a method without a body cannot have.
fn declaration(method: &ImplItemFn) -> Tokens {
    let attributes = &method.attrs;
    let mut signature = method.sig.clone();
    for input in &mut signature.inputs {
        match input {
            FnArg::Receiver(receiver) if receiver.reference.is_none() => {
                receiver.mutability = None;
            }
            FnArg::Receiver(_) => {}
            FnArg::Typed(typed) => match &mut *typed.pat {
                Pat::Ident(pattern) => {
                    pattern.by_ref = None;
                    pattern.mutability = None;
                    pattern.subpat = None;
                }
                pattern => {
                    *pattern = Pat::Wild(PatWild {
                        attrs: Vec::new(),
                        underscore_token: Default::default(),
                    });
                }
            },
        }
    }
    quote!(#(#attributes)* #signature;)
}
